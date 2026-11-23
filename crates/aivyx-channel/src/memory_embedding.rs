//! Phase 75 — embedding adapter + lazy backfill.
//!
//! `aivyx-memory` owns the [`aivyx_memory::EmbeddingHook`] seam
//! but deliberately does not depend on `aivyx-llm`. This module
//! is the bridge: it adapts the `aivyx-llm`
//! [`EmbeddingProvider`] into that hook for write-time
//! embedding, and drives the hourly backfill that re-attempts
//! any entry whose vector is missing or stale-dimensioned.

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use secrecy::SecretString;

use aivyx_llm::embedding::{
    EmbeddingProvider, OpenAiEmbeddingProvider,
};
use aivyx_memory::{EmbeddingHook, Memory};

/// Upper bound on entries embedded per backfill tick. The pass
/// scans the whole corpus (cheap, same cost as the GC scan) but
/// only embeds this many per hour so one tick never stalls on a
/// huge unembedded backlog or a slow provider — the remainder
/// is picked up on subsequent ticks.
pub const BACKFILL_BATCH: usize = 64;

/// Build the production embedding provider from the validated
/// `[embedding]` config. Returns `Err` only on transport
/// construction failure (e.g. TLS backend init) — a bad
/// base_url surfaces later as a per-call error, not here.
///
/// Blocking DNS: never call from async code: the provider's constructor checks
/// its URL with `require_local` (daemon startup calls this).
pub fn build_embedding_provider(
    cfg: &aivyx_config::EmbeddingConfig,
) -> Result<Arc<dyn EmbeddingProvider>, String> {
    let api_key: Option<SecretString> =
        cfg.api_key.as_ref().map(|s| s.value.clone());
    let provider = OpenAiEmbeddingProvider::new(
        Some(cfg.base_url.clone()),
        cfg.model.clone(),
        api_key,
        cfg.dimensions,
    )
    .map_err(|e| format!("embedding provider init failed: {e}"))?;
    Ok(Arc::new(provider))
}

/// Adapts an [`EmbeddingProvider`] into the
/// [`aivyx_memory::EmbeddingHook`] the memory write tool calls.
/// Per the hook contract, every error path collapses to `None`
/// (non-fatal — the backfill retries).
pub struct LlmEmbeddingHook {
    provider: Arc<dyn EmbeddingProvider>,
}

impl LlmEmbeddingHook {
    pub fn new(provider: Arc<dyn EmbeddingProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl EmbeddingHook for LlmEmbeddingHook {
    async fn embed_one(&self, text: &str) -> Option<Vec<f32>> {
        match self.provider.embed(&[text.to_string()]).await {
            Ok(mut vecs) if !vecs.is_empty() => Some(vecs.remove(0)),
            _ => None,
        }
    }
}

/// One bounded backfill pass. Embeds up to [`BACKFILL_BATCH`]
/// entries that currently have no vector (or whose stored
/// vector no longer matches the provider's dimensionality — a
/// model swap). Returns the number of vectors written.
///
/// Failure is non-fatal and silent-ish: a provider error ends
/// the tick with `Ok(0)` so the timer simply retries next hour
/// (the caller logs a one-liner). Idempotent: `put_vector`
/// overwrites, so a half-finished tick is safe to repeat.
pub async fn run_backfill_pass(
    memory: &Arc<dyn Memory>,
    provider: &Arc<dyn EmbeddingProvider>,
) -> Result<usize, String> {
    let dims = provider.dimensions();

    // (topic, seq) that already have a current-dimension vector.
    let have: HashSet<(String, u64)> = memory
        .load_all_vectors()
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|(_, _, v)| v.len() == dims)
        .map(|(t, s, _)| (t, s))
        .collect();

    // Enumerate every entry. `scan_prefix` with an empty prefix
    // walks all topics and — unlike `get_recent` — does not bump
    // the LRU read-stamp, so the backfill never perturbs
    // eviction order. `usize::MAX` = no per-topic truncation.
    let all = memory
        .scan_prefix("", usize::MAX)
        .await
        .map_err(|e| e.to_string())?;

    let mut topics: Vec<String> = Vec::new();
    let mut seqs: Vec<u64> = Vec::new();
    let mut bodies: Vec<String> = Vec::new();
    for (topic, entries) in &all {
        for e in entries {
            if have.contains(&(topic.clone(), e.seq)) {
                continue;
            }
            topics.push(topic.clone());
            seqs.push(e.seq);
            bodies.push(e.body.clone());
            if bodies.len() >= BACKFILL_BATCH {
                break;
            }
        }
        if bodies.len() >= BACKFILL_BATCH {
            break;
        }
    }

    if bodies.is_empty() {
        return Ok(0);
    }

    let vectors = match provider.embed(&bodies).await {
        Ok(v) => v,
        // Non-fatal: provider down / rate-limited / no key.
        // Next tick retries the same candidates.
        Err(_) => return Ok(0),
    };
    if vectors.len() != bodies.len() {
        return Ok(0);
    }

    let mut written = 0usize;
    for ((topic, seq), vector) in
        topics.into_iter().zip(seqs).zip(vectors)
    {
        if memory.put_vector(&topic, seq, vector).await.is_ok() {
            written += 1;
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_llm::embedding::EmbeddingError;
    use aivyx_memory::InMemoryMemory;

    /// Deterministic provider: maps each text to a fixed-dim
    /// vector derived from its byte sum, or fails on demand.
    struct FakeProvider {
        dims: usize,
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
            Ok(texts
                .iter()
                .map(|t| {
                    let s = t.bytes().map(|b| b as f32).sum::<f32>();
                    let mut v = vec![0.0; self.dims];
                    if !v.is_empty() {
                        v[0] = s;
                    }
                    v
                })
                .collect())
        }
        fn model(&self) -> &str {
            "fake"
        }
        fn dimensions(&self) -> usize {
            self.dims
        }
    }

    #[tokio::test]
    async fn backfill_embeds_only_missing_entries() {
        let memory: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        let s0 = memory.put("t", "alpha").await.unwrap();
        let _s1 = memory.put("t", "beta").await.unwrap();
        // s0 already embedded with the right dim → skipped.
        memory.put_vector("t", s0, vec![1.0, 0.0]).await.unwrap();

        let provider: Arc<dyn EmbeddingProvider> =
            Arc::new(FakeProvider { dims: 2, fail: false });
        let n = run_backfill_pass(&memory, &provider).await.unwrap();
        assert_eq!(n, 1, "only the un-embedded entry is backfilled");
        assert_eq!(memory.load_all_vectors().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn backfill_reembeds_stale_dimension_vectors() {
        let memory: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        let s = memory.put("t", "x").await.unwrap();
        // Old 3-dim vector; provider now emits 2-dim → stale.
        memory.put_vector("t", s, vec![9.0, 9.0, 9.0]).await.unwrap();

        let provider: Arc<dyn EmbeddingProvider> =
            Arc::new(FakeProvider { dims: 2, fail: false });
        let n = run_backfill_pass(&memory, &provider).await.unwrap();
        assert_eq!(n, 1);
        let all = memory.load_all_vectors().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].2.len(), 2, "re-embedded at the new dim");
    }

    #[tokio::test]
    async fn backfill_provider_failure_is_nonfatal() {
        let memory: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        memory.put("t", "x").await.unwrap();
        let provider: Arc<dyn EmbeddingProvider> =
            Arc::new(FakeProvider { dims: 2, fail: true });
        // Errors collapse to Ok(0); no vector written; retried
        // next tick.
        assert_eq!(
            run_backfill_pass(&memory, &provider).await.unwrap(),
            0
        );
        assert!(memory.load_all_vectors().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn backfill_is_idempotent() {
        let memory: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        memory.put("t", "a").await.unwrap();
        memory.put("t", "b").await.unwrap();
        let provider: Arc<dyn EmbeddingProvider> =
            Arc::new(FakeProvider { dims: 2, fail: false });
        assert_eq!(run_backfill_pass(&memory, &provider).await.unwrap(), 2);
        // Second pass finds nothing left to do.
        assert_eq!(run_backfill_pass(&memory, &provider).await.unwrap(), 0);
        assert_eq!(memory.load_all_vectors().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn embed_one_hook_maps_error_to_none() {
        let ok: Arc<dyn EmbeddingProvider> =
            Arc::new(FakeProvider { dims: 2, fail: false });
        let bad: Arc<dyn EmbeddingProvider> =
            Arc::new(FakeProvider { dims: 2, fail: true });
        assert!(LlmEmbeddingHook::new(ok).embed_one("hi").await.is_some());
        assert!(LlmEmbeddingHook::new(bad).embed_one("hi").await.is_none());
    }
}
