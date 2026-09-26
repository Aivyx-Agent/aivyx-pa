//! Chapter Codex (CX.1) — the persistent knowledge-wiki page store.
//!
//! One encrypted row per **canonical topic** in
//! [`aivyx_storage::KeyDomain::KnowledgeWiki`], holding the synthesized
//! [`WikiPage`] for that topic. This module owns only the *storage* of
//! pages (CRUD + an incremental-regeneration check); the synthesizer that
//! *fills* a page from memory + the LLM lands in CX.2, and the generation
//! cadence in CX.3.
//!
//! Pages are **derived** — the source of truth is always memory + the
//! co-occurrence ledger. The store is a cache: a missing or stale page
//! costs only a (re)synthesis, never correctness, and a corrupt row
//! degrades only the codex (the Studio Wiki view + the opt-in recall
//! unit), never memory or recall.
//!
//! Topics are canonicalized on the way in (the Phase 89 / Loom
//! canonicalizer) so a page keys identically to the memory entries it
//! summarizes — `deploy` / `deploying` / `Deploy` share one page.

use aivyx_memory::canonicalize_topic;
use aivyx_storage::DomainHandle;

pub use aivyx_ipc::wiki::{WikiBacklink, WikiPage, WikiPageSummary};

/// Snippet length (Unicode chars) for the Studio list rows.
pub const WIKI_SNIPPET_CHARS: usize = 160;

/// Errors from the wiki store. Mirrors the co-occurrence ledger's shape:
/// a storage-layer failure or an (de)serialization failure, both carried
/// as detail strings so the caller can log and skip.
#[derive(Debug, thiserror::Error)]
pub enum WikiStoreError {
    #[error("knowledge-wiki storage error: {0}")]
    Storage(String),
    #[error("knowledge-wiki encode/decode error: {0}")]
    Encode(String),
}

/// Persistent store for synthesized [`WikiPage`]s, keyed by canonical
/// topic in [`aivyx_storage::KeyDomain::KnowledgeWiki`].
pub struct PersistentWikiStore {
    storage: DomainHandle,
}

impl PersistentWikiStore {
    pub fn new(storage: DomainHandle) -> Self {
        Self { storage }
    }

    /// Storage key for a topic: its canonical form, as bytes.
    fn key(topic: &str) -> Vec<u8> {
        canonicalize_topic(topic).into_bytes()
    }

    /// Upsert a page. The `topic` field is canonicalized so the stored
    /// page keys consistently regardless of the caller's casing/inflection.
    pub async fn put_page(&self, page: &WikiPage) -> Result<(), WikiStoreError> {
        let canonical = canonicalize_topic(&page.topic);
        let stored = WikiPage { topic: canonical.clone(), ..page.clone() };
        let bytes = serde_json::to_vec(&stored)
            .map_err(|e| WikiStoreError::Encode(e.to_string()))?;
        self.storage
            .put(canonical.as_bytes(), &bytes)
            .await
            .map_err(|e| WikiStoreError::Storage(e.to_string()))
    }

    /// Fetch a topic's page, if one has been synthesized.
    pub async fn get_page(&self, topic: &str) -> Result<Option<WikiPage>, WikiStoreError> {
        let raw = self
            .storage
            .get(&Self::key(topic))
            .await
            .map_err(|e| WikiStoreError::Storage(e.to_string()))?;
        match raw {
            Some(bytes) => {
                let page = serde_json::from_slice(&bytes)
                    .map_err(|e| WikiStoreError::Encode(e.to_string()))?;
                Ok(Some(page))
            }
            None => Ok(None),
        }
    }

    /// Delete a topic's page (e.g. after the topic is forgotten). Idempotent.
    pub async fn delete_page(&self, topic: &str) -> Result<(), WikiStoreError> {
        self.storage
            .delete(&Self::key(topic))
            .await
            .map_err(|e| WikiStoreError::Storage(e.to_string()))
    }

    /// All pages, full. Corrupt rows are skipped (best-effort) rather than
    /// failing the whole read — one bad page must not blank the codex.
    pub async fn all_pages(&self) -> Result<Vec<WikiPage>, WikiStoreError> {
        let rows = self
            .storage
            .scan_prefix(&[])
            .await
            .map_err(|e| WikiStoreError::Storage(e.to_string()))?;
        let mut out: Vec<WikiPage> = rows
            .iter()
            .filter_map(|(_k, v)| serde_json::from_slice(v).ok())
            .collect();
        // Stable, operator-friendly order: most-recently-updated first,
        // then topic ascending for ties.
        out.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.topic.cmp(&b.topic))
        });
        Ok(out)
    }

    /// Compact list rows for the Studio Wiki index, most-recent first.
    pub async fn list_summaries(&self) -> Result<Vec<WikiPageSummary>, WikiStoreError> {
        Ok(self
            .all_pages()
            .await?
            .iter()
            .map(|p| p.to_summary(WIKI_SNIPPET_CHARS))
            .collect())
    }

    /// Incremental-regeneration check: does the topic need a (re)synthesis
    /// given the current set of contributing entry `seq`s? `true` when no
    /// page exists yet or the stored `source_fingerprint` differs from the
    /// fingerprint of `current_seqs`. A storage/decode error is treated as
    /// "needs regen" (fail toward freshness, never panic).
    pub async fn needs_regen(&self, topic: &str, current_seqs: &[u64]) -> bool {
        let want = WikiPage::fingerprint(current_seqs);
        match self.get_page(topic).await {
            Ok(Some(page)) => page.source_fingerprint != want,
            _ => true,
        }
    }
}

// ---------------------------------------------------------------------------
// WikiSynthesizer — Chapter Codex (CX.2)
// ---------------------------------------------------------------------------

use std::sync::Arc;

use aivyx_llm::{ContentBlock, LlmMessage, LlmProvider, LlmRequest, LlmStepEnd};
use aivyx_memory::{Memory, MemoryEntry};
use aivyx_core::CancellationToken;

use crate::cooccurrence_ledger::PersistentCooccurrenceLedger;

/// Tuning for page synthesis. Defaults aim for a tight, cheap page.
#[derive(Debug, Clone)]
pub struct WikiSynthConfig {
    /// Newest entries (per topic) to consolidate into the summary.
    pub max_entries: usize,
    /// Per-entry body cap (chars) in the prompt, so one long note can't
    /// blow the synthesis budget.
    pub max_entry_chars: usize,
    /// LLM token budget for the summary itself.
    pub max_tokens: u32,
    /// Graph-walk depth for backlinks (1 = direct co-occurrence siblings).
    pub backlink_hops: u32,
    pub backlink_decay: f32,
    pub backlink_min_affinity: f32,
    pub max_backlinks: usize,
}

impl Default for WikiSynthConfig {
    fn default() -> Self {
        Self {
            max_entries: 50,
            max_entry_chars: 500,
            max_tokens: 400,
            backlink_hops: 1,
            backlink_decay: 0.5,
            backlink_min_affinity: 0.0,
            max_backlinks: 8,
        }
    }
}

/// What a regeneration pass did for one topic.
#[derive(Debug, Clone, PartialEq)]
pub enum RegenOutcome {
    /// The page was up to date (fingerprint matched) — nothing rewritten.
    Skipped,
    /// The topic has no entries to summarize.
    NoEntries,
    /// A fresh page was synthesized and stored.
    Wrote(WikiPage),
}

/// Builds + refreshes [`WikiPage`]s from memory: consolidates a topic's
/// entries into an LLM summary, links it by co-occurrence, and stores it.
/// Everything is **best-effort** — any failure (no entries, an LLM error,
/// a storage hiccup) leaves the existing page (and memory/recall)
/// untouched and returns a non-fatal outcome.
pub struct WikiSynthesizer {
    memory: Arc<dyn Memory>,
    provider: Arc<dyn LlmProvider>,
    store: Arc<PersistentWikiStore>,
    ledger: Option<Arc<PersistentCooccurrenceLedger>>,
    model: String,
    config: WikiSynthConfig,
}

impl WikiSynthesizer {
    pub fn new(
        memory: Arc<dyn Memory>,
        provider: Arc<dyn LlmProvider>,
        store: Arc<PersistentWikiStore>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            memory,
            provider,
            store,
            ledger: None,
            model: model.into(),
            config: WikiSynthConfig::default(),
        }
    }

    /// Attach the co-occurrence ledger so pages get backlinks. Without it,
    /// pages synthesize fine but carry no links.
    pub fn with_ledger(mut self, ledger: Arc<PersistentCooccurrenceLedger>) -> Self {
        self.ledger = Some(ledger);
        self
    }

    pub fn with_config(mut self, config: WikiSynthConfig) -> Self {
        self.config = config;
        self
    }

    /// The consolidation system prompt — constrains the model to
    /// summarize only what the entries say, as background knowledge.
    fn system_prompt() -> &'static str {
        "You are consolidating an AI assistant's own memory notes about a \
         single topic into a short reference summary. Write 2-4 sentences \
         (or a few tight bullet points) capturing only what the notes \
         actually say — the durable facts and state of this topic. Do NOT \
         invent details not present in the notes, do NOT add new \
         instructions, and do NOT address the user. Output only the \
         summary text, no preamble."
    }

    /// Render the entries into the user prompt. Pure + testable.
    fn user_prompt(&self, topic: &str, entries: &[MemoryEntry]) -> String {
        let mut s = String::new();
        s.push_str(&format!("Topic: {topic}\n\nNotes (newest first):\n"));
        for e in entries {
            let body: String = if e.body.chars().count() > self.config.max_entry_chars {
                let head: String =
                    e.body.chars().take(self.config.max_entry_chars).collect();
                format!("{head}…")
            } else {
                e.body.clone()
            };
            // Single-line each so a note's newlines can't forge structure.
            s.push_str(&format!("- {}\n", body.replace('\n', " ")));
        }
        s.push_str("\nWrite the consolidated summary now.");
        s
    }

    /// One-shot LLM consolidation. Best-effort: any error or an empty/
    /// tool-call response yields `None` (the caller keeps the old page).
    async fn summarize(&self, topic: &str, entries: &[MemoryEntry]) -> Option<String> {
        let user = self.user_prompt(topic, entries);
        let messages = vec![LlmMessage::User {
            content: vec![ContentBlock::Text { text: user }],
        }];
        let request = LlmRequest {
            model: &self.model,
            system: Some(Self::system_prompt()),
            messages: &messages,
            tools: &[],
            max_tokens: self.config.max_tokens,
            temperature: Some(0.2),
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        let token = CancellationToken::new();
        let mut stream = self.provider.chat_stream(request, &token).await.ok()?;
        // Text-only; drain mid-stream events.
        while stream.next_event().await.ok()?.is_some() {}
        let text = match stream.finish().await.ok()? {
            LlmStepEnd::FinalMessage { text, .. } => text,
            LlmStepEnd::ToolCalls { .. } => return None,
        };
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    }

    /// Co-occurrence backlinks for a topic via Loom's multi-hop walk.
    /// Empty when no ledger is attached or the walk finds nothing.
    async fn backlinks(&self, topic: &str, now_secs: u64) -> Vec<WikiBacklink> {
        let Some(ledger) = &self.ledger else {
            return Vec::new();
        };
        let neighbors = ledger
            .neighbors_within(
                topic,
                now_secs,
                self.config.backlink_hops,
                self.config.backlink_decay,
                self.config.backlink_min_affinity,
                self.config.max_backlinks,
            )
            .await
            .unwrap_or_default();
        neighbors
            .into_iter()
            .map(|n| WikiBacklink {
                topic: n.topic,
                affinity: n.affinity,
                hops: n.hops,
            })
            .collect()
    }

    /// Make a topic's page current. Pulls its entries, skips when the
    /// fingerprint already matches (incremental), otherwise synthesizes a
    /// summary + backlinks and stores the page. Best-effort: returns
    /// [`RegenOutcome::Skipped`] on any soft failure rather than erroring.
    pub async fn regenerate(&self, topic: &str, now_secs: u64) -> RegenOutcome {
        let entries = match self.memory.get_recent(topic, self.config.max_entries).await {
            Ok(e) => e,
            Err(_) => return RegenOutcome::Skipped,
        };
        if entries.is_empty() {
            return RegenOutcome::NoEntries;
        }
        let seqs: Vec<u64> = entries.iter().map(|e| e.seq).collect();
        // Incremental: unchanged source ⇒ keep the existing page.
        if !self.store.needs_regen(topic, &seqs).await {
            return RegenOutcome::Skipped;
        }
        let Some(summary) = self.summarize(topic, &entries).await else {
            return RegenOutcome::Skipped;
        };
        let backlinks = self.backlinks(topic, now_secs).await;
        let page = WikiPage {
            topic: topic.to_string(),
            summary,
            entry_count: seqs.len() as u32,
            source_fingerprint: WikiPage::fingerprint(&seqs),
            source_seqs: seqs,
            backlinks,
            updated_at: now_secs,
        };
        match self.store.put_page(&page).await {
            Ok(()) => RegenOutcome::Wrote(page),
            Err(_) => RegenOutcome::Skipped,
        }
    }

    /// Refresh every topic's page that has gone stale, newest churn
    /// first, capped at `max_pages` (re)generations per sweep so one
    /// pass can't fire an unbounded number of LLM calls. The cap counts
    /// **writes** (the costly path), not scans — already-current topics
    /// are cheap fingerprint checks. Best-effort: a `list_topics` failure
    /// returns an empty report; per-topic failures are folded into
    /// `skipped`. Returns a [`SweepReport`] for the breadcrumb / tests.
    pub async fn sweep(&self, now_secs: u64, max_pages: usize) -> SweepReport {
        let mut report = SweepReport::default();
        // Self-heal (2026-07-04 dogfood): purge pages that ALREADY exist
        // for internal/machine topics — e.g. `loop:progress` leaked a page
        // before the classifier covered the loop's bookkeeping prefix.
        // Skipping at synthesis (below) prevents new leaks but leaves the
        // stored page listing in the Studio Wiki, recall fusion, and the
        // Praxis candidate walk forever. Best-effort: a failed delete just
        // retries next sweep.
        if let Ok(pages) = self.store.all_pages().await {
            for page in pages {
                if crate::prune_sink::is_internal_topic(&page.topic) {
                    let _ = self.store.delete_page(&page.topic).await;
                }
            }
        }
        let topics = match self.memory.list_topics().await {
            Ok(t) => t,
            Err(_) => return report,
        };
        for topic in topics {
            if report.wrote >= max_pages {
                break;
            }
            // #11 — never synthesize wiki pages from internal/machine topics
            // (the per-session `context:pruned:*` archives): they're bookkeeping,
            // not knowledge, and would pollute the knowledge base.
            if crate::prune_sink::is_internal_topic(&topic) {
                continue;
            }
            report.scanned += 1;
            match self.regenerate(&topic, now_secs).await {
                RegenOutcome::Wrote(_) => report.wrote += 1,
                RegenOutcome::Skipped => report.skipped += 1,
                RegenOutcome::NoEntries => report.no_entries += 1,
            }
        }
        report
    }
}

/// Tally of one [`WikiSynthesizer::sweep`] pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Topics examined this pass.
    pub scanned: usize,
    /// Pages (re)synthesized + stored.
    pub wrote: usize,
    /// Topics already up to date (fingerprint matched) or soft-failed.
    pub skipped: usize,
    /// Topics that turned out to have no entries.
    pub no_entries: usize,
}

/// What the daemon needs to run the wiki sweep loop: a ready
/// synthesizer plus the cadence knobs from `[wiki]`. Built by the binary
/// (it owns the storage + provider) and passed into `DaemonConfig`;
/// `None` there ⇒ no sweep (the byte-identical default).
pub struct WikiSweepConfig {
    pub synthesizer: Arc<WikiSynthesizer>,
    pub interval_secs: u64,
    pub max_pages: usize,
}

/// Background generation trigger (CX.3) — periodically sweep stale pages
/// onto the daemon's maintenance cadence. Best-effort and shutdown-aware:
/// the first immediate tick is skipped (the first sweep runs one interval
/// after startup), and the loop exits cleanly on `shutdown`. Modeled on
/// the daemon's hourly memory-GC / embedding-backfill timer.
pub async fn run_wiki_sweep_loop(
    synthesizer: Arc<WikiSynthesizer>,
    interval_secs: u64,
    max_pages: usize,
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
                let r = synthesizer.sweep(now, max_pages).await;
                if r.wrote > 0 {
                    eprintln!(
                        "aivyx-pa wiki-sweep: wrote {} page(s) ({} scanned, {} skipped)",
                        r.wrote, r.scanned, r.skipped
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
    use std::sync::Arc;

    async fn store() -> PersistentWikiStore {
        use aivyx_crypto::MasterKey;
        use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};
        let base = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
        let dir = std::path::PathBuf::from(base)
            .join(format!("aivyx-wiki-store-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let s: Arc<dyn Storage> = RedbStorage::open(
            StorageConfig::new(dir.join("store.redb")),
            MasterKey::from_raw([67u8; 32]),
        )
        .await
        .unwrap();
        PersistentWikiStore::new(s.domain(KeyDomain::KnowledgeWiki))
    }

    fn page(topic: &str, summary: &str, seqs: Vec<u64>, updated: u64) -> WikiPage {
        WikiPage {
            topic: topic.into(),
            summary: summary.into(),
            source_seqs: seqs.clone(),
            entry_count: seqs.len() as u32,
            backlinks: vec![],
            updated_at: updated,
            source_fingerprint: WikiPage::fingerprint(&seqs),
        }
    }

    #[tokio::test]
    async fn put_get_round_trips() {
        let s = store().await;
        assert!(s.get_page("deploy").await.unwrap().is_none());
        let p = page("deploy", "the deploy summary", vec![1, 2, 3], 100);
        s.put_page(&p).await.unwrap();
        let got = s.get_page("deploy").await.unwrap().expect("page present");
        assert_eq!(got.summary, "the deploy summary");
        assert_eq!(got.entry_count, 3);
    }

    #[tokio::test]
    async fn topic_is_canonicalized_on_key() {
        let s = store().await;
        s.put_page(&page("Deploying", "x", vec![1], 1)).await.unwrap();
        // A different inflection of the same canonical topic finds it.
        assert!(s.get_page("deploy").await.unwrap().is_some());
        let got = s.get_page("Deploys").await.unwrap().expect("canonical match");
        assert_eq!(got.topic, canonicalize_topic("Deploying"));
    }

    #[tokio::test]
    async fn delete_removes_the_page() {
        let s = store().await;
        s.put_page(&page("deploy", "x", vec![1], 1)).await.unwrap();
        s.delete_page("Deploy").await.unwrap(); // canonicalizes to same key
        assert!(s.get_page("deploy").await.unwrap().is_none());
        // Idempotent — deleting a missing page is fine.
        s.delete_page("deploy").await.unwrap();
    }

    #[tokio::test]
    async fn list_orders_most_recent_first() {
        let s = store().await;
        s.put_page(&page("old", "o", vec![1], 10)).await.unwrap();
        s.put_page(&page("new", "n", vec![2], 30)).await.unwrap();
        s.put_page(&page("mid", "m", vec![3], 20)).await.unwrap();
        let rows = s.list_summaries().await.unwrap();
        let topics: Vec<&str> = rows.iter().map(|r| r.topic.as_str()).collect();
        assert_eq!(topics, vec!["new", "mid", "old"]);
    }

    #[tokio::test]
    async fn needs_regen_tracks_the_source_fingerprint() {
        let s = store().await;
        // No page yet → needs regen.
        assert!(s.needs_regen("deploy", &[1, 2]).await);
        s.put_page(&page("deploy", "x", vec![1, 2], 5)).await.unwrap();
        // Same entry set → up to date.
        assert!(!s.needs_regen("deploy", &[2, 1]).await); // order-independent
        // A new entry → stale.
        assert!(s.needs_regen("deploy", &[1, 2, 3]).await);
    }

    // ---- CX.2 — WikiSynthesizer -------------------------------------

    use aivyx_llm::{
        LlmError, LlmProvider, LlmRequest, LlmStepEnd, LlmStream, LlmStreamEvent, LlmUsage,
    };
    use aivyx_memory::InMemoryMemory;
    use async_trait::async_trait;
    use aivyx_core::CancellationToken;

    /// A provider that returns one scripted `FinalMessage`, or fails the
    /// stream open when `fail` is set.
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

    fn synth(
        memory: Arc<dyn Memory>,
        store: Arc<PersistentWikiStore>,
        text: &str,
        fail: bool,
    ) -> WikiSynthesizer {
        WikiSynthesizer::new(
            memory,
            Arc::new(ScriptedProvider { text: text.into(), fail }),
            store,
            "fake-model",
        )
    }

    #[tokio::test]
    async fn regenerate_writes_a_page_from_entries() {
        let mem: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        mem.put("deploy", "we ship via the ci pipeline").await.unwrap();
        mem.put("deploy", "rollbacks use the previous image tag").await.unwrap();
        let store = Arc::new(store().await);
        let s = synth(Arc::clone(&mem), Arc::clone(&store), "Deploy ships via CI; rollback by image tag.", false);

        let outcome = s.regenerate("deploy", 1000).await;
        match outcome {
            RegenOutcome::Wrote(p) => {
                assert_eq!(p.summary, "Deploy ships via CI; rollback by image tag.");
                assert_eq!(p.entry_count, 2);
                assert_eq!(p.updated_at, 1000);
            }
            other => panic!("expected Wrote, got {other:?}"),
        }
        // And it's persisted.
        assert!(store.get_page("deploy").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn regenerate_is_incremental_second_pass_skips() {
        let mem: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        mem.put("deploy", "note one").await.unwrap();
        let store = Arc::new(store().await);
        let s = synth(Arc::clone(&mem), Arc::clone(&store), "a summary", false);

        assert!(matches!(s.regenerate("deploy", 1).await, RegenOutcome::Wrote(_)));
        // No new entries → fingerprint unchanged → skipped.
        assert_eq!(s.regenerate("deploy", 2).await, RegenOutcome::Skipped);
        // A new entry → regenerates.
        mem.put("deploy", "note two").await.unwrap();
        assert!(matches!(s.regenerate("deploy", 3).await, RegenOutcome::Wrote(_)));
    }

    #[tokio::test]
    async fn regenerate_no_entries_is_noentries() {
        let mem: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        let store = Arc::new(store().await);
        let s = synth(mem, Arc::clone(&store), "x", false);
        assert_eq!(s.regenerate("never-written", 1).await, RegenOutcome::NoEntries);
        assert!(store.get_page("never-written").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn regenerate_llm_failure_skips_without_writing() {
        let mem: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        mem.put("deploy", "note").await.unwrap();
        let store = Arc::new(store().await);
        let s = synth(Arc::clone(&mem), Arc::clone(&store), "", true); // provider fails

        assert_eq!(s.regenerate("deploy", 1).await, RegenOutcome::Skipped);
        // Best-effort: no broken page written.
        assert!(store.get_page("deploy").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn regenerate_attaches_cooccurrence_backlinks() {
        use crate::cooccurrence_ledger::PersistentCooccurrenceLedger;
        use aivyx_crypto::MasterKey;
        use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};
        let base = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
        let dir = std::path::PathBuf::from(base)
            .join(format!("aivyx-wiki-synth-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let st: Arc<dyn Storage> = RedbStorage::open(
            StorageConfig::new(dir.join("s.redb")),
            MasterKey::from_raw([68u8; 32]),
        )
        .await
        .unwrap();
        let cooc = Arc::new(PersistentCooccurrenceLedger::new(
            st.domain(KeyDomain::CooccurrenceLedger),
        ));
        cooc.record_window(&[(("deploy".into(), "ci".into()), 6.0)], 100)
            .await
            .unwrap();
        let wiki_store = Arc::new(PersistentWikiStore::new(st.domain(KeyDomain::KnowledgeWiki)));

        let mem: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        mem.put("deploy", "ships via ci").await.unwrap();

        let s = synth(mem, Arc::clone(&wiki_store), "summary", false)
            .with_ledger(Arc::clone(&cooc));
        match s.regenerate("deploy", 100).await {
            RegenOutcome::Wrote(p) => {
                assert_eq!(p.backlinks.len(), 1);
                assert_eq!(p.backlinks[0].topic, "ci");
                assert_eq!(p.backlinks[0].hops, 1);
            }
            other => panic!("expected Wrote, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn sweep_writes_all_stale_then_skips_on_second_pass() {
        let mem: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        mem.put("deploy", "ships via ci").await.unwrap();
        mem.put("billing", "stripe webhooks").await.unwrap();
        let store = Arc::new(store().await);
        let s = synth(Arc::clone(&mem), Arc::clone(&store), "summary", false);

        let first = s.sweep(100, 50).await;
        assert_eq!(first.scanned, 2);
        assert_eq!(first.wrote, 2);
        assert_eq!(first.skipped, 0);

        // Nothing changed → all skipped.
        let second = s.sweep(200, 50).await;
        assert_eq!(second.scanned, 2);
        assert_eq!(second.wrote, 0);
        assert_eq!(second.skipped, 2);

        // Both pages exist.
        assert_eq!(store.list_summaries().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn sweep_skips_internal_context_pruned_topics() {
        // #11 — the wiki sweep must not synthesize pages from the per-session
        // `context:pruned:*` archives (machine bookkeeping, not knowledge).
        let mem: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        mem.put("aviation", "VFR means visual flight rules").await.unwrap();
        mem.put("context:pruned:abc-123", "5 messages were pruned").await.unwrap();
        let store = Arc::new(store().await);
        let s = synth(Arc::clone(&mem), Arc::clone(&store), "summary", false);

        let r = s.sweep(100, 50).await;
        assert_eq!(r.scanned, 1, "only the real topic is scanned");
        assert_eq!(r.wrote, 1);
        let pages = store.list_summaries().await.unwrap();
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].topic, "aviation");
        assert!(!pages.iter().any(|p| p.topic.starts_with("context:pruned:")));
    }

    #[tokio::test]
    async fn sweep_caps_writes_per_pass() {
        let mem: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        for t in ["a", "b", "c"] {
            mem.put(t, "note").await.unwrap();
        }
        let store = Arc::new(store().await);
        let s = synth(Arc::clone(&mem), Arc::clone(&store), "summary", false);

        // Cap at 2 writes → only 2 pages this pass.
        let r = s.sweep(1, 2).await;
        assert_eq!(r.wrote, 2);
        // A second pass writes the remaining one (the first two now skip).
        let r2 = s.sweep(2, 2).await;
        assert_eq!(r2.wrote, 1);
        assert_eq!(store.list_summaries().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn sweep_loop_first_tick_skipped_and_cancels() {
        // The loop skips the immediate tick, so a sweep doesn't run
        // before the first interval; cancelling returns promptly.
        let mem: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        mem.put("deploy", "note").await.unwrap();
        let store = Arc::new(store().await);
        let s = Arc::new(synth(Arc::clone(&mem), Arc::clone(&store), "summary", false));
        let shutdown = CancellationToken::new();
        let handle = tokio::spawn(run_wiki_sweep_loop(
            Arc::clone(&s),
            3600,
            10,
            shutdown.clone(),
        ));
        // No page yet (first tick skipped, interval is an hour).
        assert!(store.get_page("deploy").await.unwrap().is_none());
        shutdown.cancel();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn user_prompt_lists_entries_and_caps_bodies() {
        let mem: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        let cfg = WikiSynthConfig { max_entry_chars: 5, ..Default::default() };
        let s = WikiSynthesizer::new(
            Arc::clone(&mem),
            Arc::new(ScriptedProvider { text: "x".into(), fail: false }),
            Arc::new(store().await),
            "m",
        )
        .with_config(cfg);
        let entries = vec![MemoryEntry {
            topic: "deploy".into(),
            body: "abcdefghij".into(),
            seq: 1,
            created_at_secs: 0,
            last_read_at_secs: 0,
        }];
        let p = s.user_prompt("deploy", &entries);
        assert!(p.contains("Topic: deploy"));
        assert!(p.contains("abcde…"), "body capped at 5 chars: {p}");
    }
}
