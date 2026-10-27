//! Cross-encoder reranking - part of the configurable-retrieval-pipeline
//! design (`docs/design/2026-06-06-configurable-retrieval-pipeline.md`).
//!
//! A cross-encoder scores a `(query, doc)` pair *jointly* (both texts in one
//! transformer pass), which is more precise than the bi-encoder cosine used for
//! first-stage retrieval but far more expensive: one forward per candidate, no
//! precomputed vectors. It runs as an optional second stage over the top-K
//! fused candidates.
//!
//! ## Pieces
//!
//! - [`Reranker`] - the port. Mirrors `EmbeddingModel`: `Send + Sync`, scores a
//!   batch of docs against one query, higher = more relevant.
//! - [`MockReranker`] - deterministic lexical-overlap scorer for host-
//!   independent tests (the real model needs network + a CPU that can run it).
//! - [`CROSS_ENCODERS`] - the one cross-encoder registry both backends read.
//!   Candle serves every entry; ONNX serves an entry only when its
//!   `onnx_parity` is set, i.e. an `otto ignored` test proves the ONNX scores
//!   rank the same as Candle's (`search::rerank::onnx::tests`).
//! - `CandleCrossEncoder` (feature `vec-candle`): BertModel encoder +
//!   (optional) pooler + a linear classification head, built from scratch
//!   because candle-transformers exposes only `BertModel` (CLS-pooled
//!   embeddings), not a sequence-classification head.
//! - `OnnxCrossEncoder` (feature `vec-onnx`): the repo's own ONNX export,
//!   AVX2+FMA only, gated by `vault::cpu` before any `ort` call.
//! - [`get_or_load_reranker`] resolves the backend through the embedding
//!   resolution table (`vault::embedding::backend::resolve_with_parity`) with
//!   the registry's parity bit, then loads and caches one model per process.
//!
//! ## Honest status on a CPU without AVX2+FMA
//!
//! On a CPU without AVX2+FMA (`talos`) the ONNX runtime aborts, so there this is a
//! Candle scalar-backend pipeline at ~150-300 ms **per pair**. Reranking 50
//! unbatched candidates is ~7.5-15 s - past any interactive budget. The stage
//! is therefore **off by default** and gated by a warmup latency probe (in
//! oracle's `run_pipeline`) that fails open to the fused order when the
//! projected batch exceeds the configured budget. The `CandleCrossEncoder`
//! compiles and follows the `BertForSequenceClassification` architecture, but
//! is **not exercised in CI** (no model fetch there); the `otto ignored`
//! ordering test scores fixed pairs with both cross-encoders, and retrieval
//! quality is measured with `sb oracle eval`.

use eyre::Result;
#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
use std::sync::{Arc, OnceLock, RwLock};

#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
use crate::cpu::CpuFeatures;
#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
use crate::embedding::backend::{Backend, InferenceConfig, resolve_with_parity_logged};

/// Operator-facing id of the default cross-encoder (the oracle config
/// default, `retrieval.rerank.model`).
pub const RERANK_MODEL_ID: &str = "ms-marco-MiniLM-L6-v2";

/// One cross-encoder the backends can serve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrossEncoderSpec {
    /// Operator-facing id (`retrieval.rerank.model` in oracle.yml).
    pub id: &'static str,
    /// HuggingFace Hub repo both backends download from.
    pub hf_repo: &'static str,
    /// `true` only when the `otto ignored` ordering test proves ONNX ranks
    /// fixed pairs in Candle's order with every score within 1e-3. ONNX
    /// refuses an entry without it.
    pub onnx_parity: bool,
}

/// Every cross-encoder the registry knows. The repo id is the canonical
/// `ms-marco-MiniLM-L6-v2`; the older `ms-marco-MiniLM-L-6-v2` redirects to it.
pub const CROSS_ENCODERS: &[CrossEncoderSpec] = &[CrossEncoderSpec {
    id: RERANK_MODEL_ID,
    hf_repo: "cross-encoder/ms-marco-MiniLM-L6-v2",
    onnx_parity: true,
}];

/// Look up a cross-encoder by id; `None` if the registry lacks it.
pub fn cross_encoder_spec(id: &str) -> Option<&'static CrossEncoderSpec> {
    CROSS_ENCODERS.iter().find(|spec| spec.id == id)
}

/// [`cross_encoder_spec`], or an error naming what the registry serves.
#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
fn cross_encoder(id: &str) -> Result<&'static CrossEncoderSpec> {
    cross_encoder_spec(id).ok_or_else(|| {
        eyre::eyre!(
            "unknown rerank model {id:?} (registry serves {:?})",
            CROSS_ENCODERS.iter().map(|spec| spec.id).collect::<Vec<_>>()
        )
    })
}

/// The registry entry for `id` if ONNX may serve it, else an error naming
/// the model.
#[cfg(feature = "vec-onnx")]
fn onnx_cross_encoder(id: &str) -> Result<&'static CrossEncoderSpec> {
    let spec = cross_encoder(id)?;
    if !spec.onnx_parity {
        eyre::bail!("no ONNX parity proof for rerank model {id:?}");
    }
    Ok(spec)
}

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
/// per-pair (one-doc) cost: linear in `n`.
///
/// NO thread division. The candle cross-encoder runs ONE batched forward pass
/// over all `n` docs, and that single forward already saturates every core - so
/// the one-doc warmup probe also already used all cores. Dividing the batch by
/// `threads` (the old model) assumed embarrassingly-parallel independent pairs,
/// which is false here: the probe could pass while the real batch blew the
/// budget by up to `threads`x. `per_pair_ms * n` is the honest upper bound.
/// Pure, so the latency-budget decision stays unit-testable without timing.
pub fn project_batch_ms(per_pair_ms: f64, n: usize) -> f64 {
    per_pair_ms * n as f64
}

#[cfg(any(test, feature = "test-util"))]
/// Deterministic test reranker: scores by lexical token overlap between query
/// and doc (count of query whitespace-tokens present in the doc). No model
/// load, stable across runs - mirrors `MockEmbedder`'s role for embeddings.
pub struct MockReranker {
    model_id: String,
}

#[cfg(any(test, feature = "test-util"))]
impl MockReranker {
    pub fn new() -> Self {
        Self {
            model_id: "mock-cross-encoder".to_string(),
        }
    }
}

#[cfg(any(test, feature = "test-util"))]
impl Default for MockReranker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "test-util"))]
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

/// Resolve the backend for rerank model `model_id`: the embedding resolution
/// table with the cross-encoder registry's parity bit, logged at INFO.
/// An id outside the registry is an error under every backend choice.
#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
pub fn resolve_rerank_backend(
    requested: crate::embedding::BackendChoice,
    cpu: &CpuFeatures,
    model_id: &str,
) -> Result<Backend> {
    let spec = cross_encoder(model_id)?;
    resolve_with_parity_logged(requested, cpu, model_id, spec.onnx_parity)
}

/// Load `model_id` on the resolved `backend`. `threads` reaches the ONNX
/// session as intra-op threads (`0` = the ONNX default); the Candle
/// cross-encoder has no thread knob and ignores it.
#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
pub fn load_reranker(model_id: &str, backend: Backend, threads: usize, cpu: &CpuFeatures) -> Result<Box<dyn Reranker>> {
    log::debug!("rerank::load_reranker: model={model_id} backend={backend} threads={threads}");
    match backend {
        #[cfg(feature = "vec-candle")]
        Backend::Candle => {
            let _ = (threads, cpu);
            Ok(Box::new(candle::CandleCrossEncoder::load(model_id)?))
        }
        #[cfg(feature = "vec-onnx")]
        Backend::Onnx => Ok(Box::new(onnx::OnnxCrossEncoder::load(model_id, threads, cpu)?)),
        #[allow(unreachable_patterns)]
        other => eyre::bail!("this binary was built without the {other} rerank backend"),
    }
}

/// Idempotent prefetch of `model_id`'s files for `backend` into the hf-hub
/// cache, so the first enabled query does not block on a cold download.
#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
pub fn prefetch_reranker(model_id: &str, backend: Backend) -> Result<()> {
    log::debug!("rerank::prefetch_reranker: model={model_id} backend={backend}");
    match backend {
        #[cfg(feature = "vec-candle")]
        Backend::Candle => candle::CandleCrossEncoder::prefetch(model_id),
        #[cfg(feature = "vec-onnx")]
        Backend::Onnx => onnx::OnnxCrossEncoder::prefetch(model_id),
        #[allow(unreachable_patterns)]
        other => eyre::bail!("this binary was built without the {other} rerank backend"),
    }
}

#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
type RegistryEntry = (String, Arc<dyn Reranker>);

/// Process-local cache of loaded cross-encoders, keyed by model id. The
/// backend is fixed per process (config plus CPU), so the key needs no
/// backend part.
#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
static RERANK_REGISTRY: OnceLock<RwLock<Vec<RegistryEntry>>> = OnceLock::new();

#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
fn registry() -> &'static RwLock<Vec<RegistryEntry>> {
    RERANK_REGISTRY.get_or_init(|| RwLock::new(Vec::new()))
}

/// Lazy-load + cache the cross-encoder for `model_id`. The first call
/// resolves `inference.backend` against `cpu` (one INFO line) and pays the
/// model load; later calls hit the cache.
#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
pub fn get_or_load_reranker(
    model_id: &str,
    inference: &InferenceConfig,
    cpu: &CpuFeatures,
) -> Result<Arc<dyn Reranker>> {
    {
        let guard = registry()
            .read()
            .map_err(|_| eyre::eyre!("RERANK_REGISTRY read lock poisoned"))?;
        if let Some(e) = guard.iter().find(|(k, _)| k == model_id) {
            return Ok(e.1.clone());
        }
    }
    let backend = resolve_rerank_backend(inference.backend, cpu, model_id)?;
    let model: Arc<dyn Reranker> = Arc::from(load_reranker(model_id, backend, inference.threads, cpu)?);
    let mut guard = registry()
        .write()
        .map_err(|_| eyre::eyre!("RERANK_REGISTRY write lock poisoned"))?;
    if let Some(e) = guard.iter().find(|(k, _)| k == model_id) {
        return Ok(e.1.clone());
    }
    guard.push((model_id.to_string(), model.clone()));
    Ok(model)
}

#[cfg(feature = "vec-candle")]
pub use candle::CandleCrossEncoder;
#[cfg(feature = "vec-onnx")]
pub use onnx::OnnxCrossEncoder;

#[cfg(feature = "vec-candle")]
mod candle;
#[cfg(feature = "vec-onnx")]
mod onnx;

#[cfg(test)]
mod tests;
