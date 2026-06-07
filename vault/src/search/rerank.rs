//! Cross-encoder reranking - Phase 4 of the configurable-retrieval-pipeline
//! design (`docs/design/2026-06-06-configurable-retrieval-pipeline.md`).
//!
//! A cross-encoder scores a `(query, doc)` pair *jointly* (both texts in one
//! transformer pass), which is more precise than the bi-encoder cosine used for
//! first-stage retrieval but far more expensive: one forward per candidate, no
//! precomputed vectors. It runs as an optional second stage over the top-K
//! fused candidates.
//!
//! ## Three pieces
//!
//! - [`Reranker`] - the port. Mirrors `EmbeddingModel`: `Send + Sync`, scores a
//!   batch of docs against one query, higher = more relevant.
//! - [`MockReranker`] - deterministic lexical-overlap scorer for host-
//!   independent tests (the real model needs network + a CPU that can run it).
//! - [`CandleCrossEncoder`] (feature `vec-candle`) - the production scorer:
//!   BertModel encoder + (optional) pooler + a linear classification head,
//!   built from scratch because candle-transformers exposes only `BertModel`
//!   (CLS-pooled embeddings), not a sequence-classification head.
//!
//! ## Honest status on the daemon host
//!
//! The daemon host (`desk`) is AVX-only; fastembed aborts there, so this is a
//! Candle scalar-backend pipeline at ~150-300 ms **per pair**. Reranking 50
//! unbatched candidates is ~7.5-15 s - past any interactive budget. The stage
//! is therefore **off by default** and gated by a warmup latency probe (in
//! oracle's `run_pipeline`) that fails open to the fused order when the
//! projected batch exceeds the configured budget. The `CandleCrossEncoder`
//! compiles and follows the `BertForSequenceClassification` architecture, but
//! is **not exercised in CI** (no model fetch, no AVX2 there); its correctness
//! is validated by enabling it against `sb oracle eval`, not by a unit test.

use eyre::Result;

/// Port for a cross-encoder reranker. `score` returns one relevance score per
/// doc, in input order; higher means more relevant to `query`.
pub trait Reranker: Send + Sync {
    /// Stable model identifier (e.g. `"ms-marco-MiniLM-L6-v2"`).
    fn model_id(&self) -> &str;

    /// Score each doc against `query`. Returns `docs.len()` scores in order.
    fn score(&self, query: &str, docs: &[&str]) -> Result<Vec<f32>>;
}

/// Reorder `(path, text)` candidates by descending rerank score, breaking ties
/// by path ascending (deterministic). Pure over a [`Reranker`], so the reorder
/// behavior is unit-testable with [`MockReranker`] on any host.
pub fn rerank_paths(reranker: &dyn Reranker, query: &str, items: &[(String, String)]) -> Result<Vec<String>> {
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let docs: Vec<&str> = items.iter().map(|(_, t)| t.as_str()).collect();
    let scores = reranker.score(query, &docs)?;
    if scores.len() != items.len() {
        eyre::bail!("reranker returned {} scores for {} docs", scores.len(), items.len());
    }
    let mut scored: Vec<(String, f32)> = items.iter().map(|(p, _)| p.clone()).zip(scores).collect();
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    Ok(scored.into_iter().map(|(p, _)| p).collect())
}

/// Project the wall-clock cost (ms) of reranking `n` pairs from a measured
/// per-pair cost and the available parallelism: pairs run in `ceil(n/threads)`
/// waves. Pure, so the latency-budget decision is unit-testable without timing.
pub fn project_batch_ms(per_pair_ms: f64, n: usize, threads: usize) -> f64 {
    if n == 0 {
        return 0.0;
    }
    let threads = threads.max(1);
    let waves = n.div_ceil(threads) as f64;
    per_pair_ms * waves
}

/// Deterministic test reranker: scores by lexical token overlap between query
/// and doc (count of query whitespace-tokens present in the doc). No model
/// load, stable across runs - mirrors `MockEmbedder`'s role for embeddings.
pub struct MockReranker {
    model_id: String,
}

impl MockReranker {
    pub fn new() -> Self {
        Self {
            model_id: "mock-cross-encoder".to_string(),
        }
    }
}

impl Default for MockReranker {
    fn default() -> Self {
        Self::new()
    }
}

impl Reranker for MockReranker {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn score(&self, query: &str, docs: &[&str]) -> Result<Vec<f32>> {
        let q_tokens: Vec<&str> = query.split_whitespace().collect();
        Ok(docs
            .iter()
            .map(|doc| {
                let doc_lc = doc.to_lowercase();
                q_tokens.iter().filter(|t| doc_lc.contains(&t.to_lowercase())).count() as f32
            })
            .collect())
    }
}

#[cfg(feature = "vec-candle")]
pub use candle::{CandleCrossEncoder, get_or_load_reranker, prefetch_reranker};

#[cfg(feature = "vec-candle")]
mod candle;

#[cfg(test)]
mod tests;
