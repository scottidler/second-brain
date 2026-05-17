//! fastembed-backed embedding adapter (gated behind `vec-fastembed`).
//!
//! Phase 1 of `docs/design/2026-05-17-candle-embedding-backend.md`
//! relocated this code out of `embedding.rs` so the default Candle
//! backend and the legacy fastembed backend coexist behind separate
//! Cargo features.
//!
//! See the design doc's Tier ladder for when this path is selected: an
//! AVX2+ machine that wants ONNX MLAS performance builds with
//! `--no-default-features --features vec-fastembed`. On pre-AVX2 CPUs
//! loading the model SIGILLs (exit 132); this code refuses to load on
//! such CPUs and points the caller at the `vec-candle` feature.
//!
//! The trait surface (`EmbeddingModel`) is unchanged from what
//! `vault::embedding` defines; this module only provides one impl.

use eyre::{Result, WrapErr};
use std::sync::Mutex;

use super::{BGE_SMALL_EN_V15_DIM, EmbeddingModel};

/// Canonical model identifier when the active backend is fastembed/ONNX.
pub const FASTEMBED_MODEL_VERSION: &str = "bge-small-en-v1.5-fastembed";

/// fastembed-backed adapter that pins `bge-small-en-v1.5`.
///
/// Loading the model is expensive (~100 MB cache + ~1-2 s session
/// initialization), so callers should hold a single instance for the
/// lifetime of the work. Cortex's daemon keeps one across embed ticks;
/// oracle keeps one process-local instance behind the registry.
///
/// `fastembed::TextEmbedding::embed` takes `&mut self`, so the inner
/// model lives behind a `Mutex` to keep the public surface `&self`.
/// fastembed's ONNX MLAS spawns its own internal threads, so no
/// additional pooling is needed here.
pub struct FastEmbedModel {
    inner: Mutex<fastembed::TextEmbedding>,
    dim: usize,
    model_version: String,
}

impl FastEmbedModel {
    /// Load the default `bge-small-en-v1.5` model.
    ///
    /// Cost: first call downloads ~100 MB to the fastembed cache (HF Hub)
    /// and initializes the ONNX runtime session (~1-2 s). Subsequent
    /// calls within the same process should reuse the returned handle.
    ///
    /// Pre-AVX2 guard: pyke's bundled ONNX Runtime crashes (SIGILL) on
    /// CPUs without AVX2. Detect that case up-front and surface a clear
    /// error pointing at the `vec-candle` feature, rather than letting
    /// the inference engine die mid-warmup.
    pub fn load() -> Result<Self> {
        #[cfg(target_arch = "x86_64")]
        {
            if !std::arch::is_x86_feature_detected!("avx2") {
                eyre::bail!(
                    "fastembed's bundled ONNX Runtime requires AVX2 but this CPU \
                     does not advertise it; rebuild with \
                     `--no-default-features --features vec-candle` (or just \
                     `--features vec`, which defaults to the Candle backend)."
                );
            }
        }
        let options = fastembed::TextInitOptions::new(fastembed::EmbeddingModel::BGESmallENV15);
        let model = fastembed::TextEmbedding::try_new(options)
            .map_err(|e| eyre::eyre!("failed to load bge-small-en-v1.5: {e}"))
            .wrap_err("FastEmbedModel::load")?;
        Ok(Self {
            inner: Mutex::new(model),
            dim: BGE_SMALL_EN_V15_DIM,
            model_version: FASTEMBED_MODEL_VERSION.to_string(),
        })
    }
}

impl EmbeddingModel for FastEmbedModel {
    fn dim(&self) -> usize {
        self.dim
    }

    fn model_version(&self) -> &str {
        &self.model_version
    }

    fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        let vectors = self.embed_batch(&[text])?;
        vectors
            .into_iter()
            .next()
            .ok_or_else(|| eyre::eyre!("fastembed returned 0 embeddings for 1 input; this is a model bug"))
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut model = self
            .inner
            .lock()
            .map_err(|_| eyre::eyre!("FastEmbedModel mutex poisoned"))?;
        let owned: Vec<String> = texts.iter().map(|t| (*t).to_string()).collect();
        let embeddings = model
            .embed(owned, None)
            .map_err(|e| eyre::eyre!("fastembed embed call failed: {e}"))?;
        Ok(embeddings)
    }
}
