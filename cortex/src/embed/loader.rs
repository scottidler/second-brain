//! Backend resolution and model load for the embed loop.
//!
//! `embed.backend` and the detected CPU pick the backend
//! (`vault::embedding::resolve_backend`); `embed.workers` is handed to the
//! backend as its thread count (Candle replicas, ONNX intra-op threads).
//! The actual load is a parameter so a test can observe the resolved backend
//! without downloading a model.

use eyre::{Context, Result};
use vault::cpu::CpuFeatures;
use vault::embedding::{Backend, EmbeddingModel, resolve_backend_logged};

use crate::config::EmbedConfig;

/// A loaded embedding model and the backend serving it.
pub struct LoadedModel {
    pub backend: Backend,
    pub model: Box<dyn EmbeddingModel>,
}

/// Resolve `config.backend` for `model_version` on `cpu`, log the resolution
/// at INFO, then call `load` with the resolved backend and `config.workers`.
///
/// A resolution error (e.g. `onnx` pinned on a CPU without AVX2) is returned,
/// never silently replaced by Candle.
pub fn load_embedder<L>(config: &EmbedConfig, model_version: &str, cpu: &CpuFeatures, load: L) -> Result<LoadedModel>
where
    L: FnOnce(&str, Backend, usize, &CpuFeatures) -> Result<Box<dyn EmbeddingModel>>,
{
    log::debug!(
        "cortex::embed::load_embedder: model={model_version} backend={} workers={}",
        config.backend,
        config.workers
    );
    let backend = resolve_backend_logged(config.backend, cpu, model_version)
        .wrap_err("embed.backend cannot serve the pinned model on this CPU")?;
    let model = load(model_version, backend, config.workers, cpu)
        .wrap_err_with(|| format!("failed to load embedding model {model_version} on {backend}"))?;
    Ok(LoadedModel { backend, model })
}

#[cfg(test)]
mod tests;
