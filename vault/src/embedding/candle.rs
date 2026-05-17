//! Candle-backed embedding adapter (gated behind `vec-candle`).
//!
//! Implements `EmbeddingModel` for `BAAI/bge-small-en-v1.5` using a pure-
//! Rust BertModel from `candle-transformers`. No AVX2 floor; works on any
//! x86_64 CPU. Tier ladder (see `docs/design/2026-05-17-candle-embedding-backend.md`):
//!
//! - Tier 0 (pre-AVX2): scalar Candle, ~150-300 ms / call.
//! - Tier 1 (AVX2): rebuild with `RUSTFLAGS="-C target-cpu=native"`.
//! - Tier 2 (AVX2 + MKL): add `candle-core/mkl` feature.
//!
//! Production callers obtain an instance via `super::get_or_load_model`;
//! a process holds a single `Arc<CandleBertModel>` for the lifetime of
//! the work (cortex's daemon, oracle's lazy registry).

use eyre::Result;

use super::EmbeddingModel;

/// Canonical model identifier when the active backend is Candle.
pub const CANDLE_MODEL_VERSION: &str = "bge-small-en-v1.5-candle";

/// Embedding dimensionality of `BAAI/bge-small-en-v1.5`.
pub const DIM: usize = 384;

/// Skeleton struct - the full implementation lands in Phase 2.
pub struct CandleBertModel {
    model_version: String,
}

impl CandleBertModel {
    /// Download (if needed) and load `BAAI/bge-small-en-v1.5` from the
    /// HuggingFace Hub. Cache lives under hf-hub's default location
    /// (`~/.cache/huggingface/hub/`) so it survives binary upgrades.
    pub fn load() -> Result<Self> {
        Ok(Self {
            model_version: CANDLE_MODEL_VERSION.to_string(),
        })
    }

    /// Idempotent prefetch: download the three required files (config,
    /// tokenizer, weights) into the hf-hub cache without instantiating
    /// the model. Used by `cortex embed --prefetch-model`.
    pub fn prefetch_bge_small() -> Result<()> {
        Ok(())
    }
}

impl EmbeddingModel for CandleBertModel {
    fn dim(&self) -> usize {
        DIM
    }

    fn model_version(&self) -> &str {
        &self.model_version
    }

    fn embed_one(&self, _text: &str) -> Result<Vec<f32>> {
        eyre::bail!("CandleBertModel::embed_one not yet implemented (Phase 2)")
    }

    fn embed_batch(&self, _texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        eyre::bail!("CandleBertModel::embed_batch not yet implemented (Phase 2)")
    }
}
