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
//!
//! Pooling: `BertModel::forward` returns `[batch, seq, 384]`. We CLS-pool
//! by taking token 0 only (the canonical BGE recipe per the model card;
//! mean pooling, which the upstream candle BERT example demonstrates for
//! the Chinese variant, is wrong for the English BGE family). After
//! pooling we L2-normalize so cosine similarity reduces to a dot product.

use candle_core::{DType, Device, IndexOp, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config, DTYPE};
use eyre::{Result, WrapErr};
use hf_hub::api::sync::Api;
use rayon::prelude::*;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

use super::EmbeddingModel;

/// HuggingFace Hub repo id for the model we pin.
const MODEL_REPO: &str = "BAAI/bge-small-en-v1.5";

/// Canonical model identifier when the active backend is Candle.
pub const CANDLE_MODEL_VERSION: &str = "bge-small-en-v1.5-candle";

/// Embedding dimensionality of `BAAI/bge-small-en-v1.5`.
pub const DIM: usize = 384;

/// Hard token limit declared by the bge-small-en-v1.5 model card.
const MAX_SEQ_LEN: usize = 512;

/// Cap the internal worker pool. Past 8 the per-replica RAM and the
/// scaling efficiency stop being worth it on the workloads this stack
/// produces (~1346 notes one-shot, ~20/day steady-state).
const MAX_WORKERS: usize = 8;

/// Fallback worker count when neither config nor the platform supplies
/// a usable parallelism hint.
const DEFAULT_WORKERS: usize = 1;

/// Candle-backed BertModel that produces `bge-small-en-v1.5` embeddings.
///
/// `BertModel::forward` takes `&self`, but Candle's tensor pipeline
/// shares mutable internal buffers across calls (notably the encoder's
/// attention-mask broadcasts), so it is not safe to call concurrently
/// from multiple threads on the same instance. Each `Inner` lives behind
/// its own `Mutex`; a single `embed_batch` call fans across the replicas
/// via rayon. Replica count is bounded by `MAX_WORKERS` and the caller's
/// configured worker count.
///
/// Backend identification (`CANDLE_MODEL_VERSION`) is independent of
/// replica count and kernel choice; switching scalar → AVX2 → MKL does
/// not change the model_version string.
pub struct CandleBertModel {
    replicas: Vec<Mutex<Inner>>,
    next_replica: AtomicUsize,
    model_version: String,
}

struct Inner {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
    pad_token_id: u32,
}

impl CandleBertModel {
    /// Download (if needed) and load `BAAI/bge-small-en-v1.5` with the
    /// default worker count (`min(MAX_WORKERS, available_parallelism)`).
    /// Most callers go through `load_with_workers` to honor their config.
    pub fn load() -> Result<Self> {
        Self::load_with_workers(default_worker_count())
    }

    /// Download (if needed) and load `BAAI/bge-small-en-v1.5` with an
    /// explicit replica count. `workers` is clamped to `[1, MAX_WORKERS]`.
    ///
    /// Each replica loads from the same on-disk `model.safetensors` via
    /// `VarBuilder::from_mmaped_safetensors`; the OS page cache shares
    /// the underlying physical pages across replicas so total RSS stays
    /// at one model's worth (~133 MB) regardless of N. We verify that
    /// invariant empirically once we deploy; if RSS scales with N the
    /// implementer must drop back to `min(4, num_physical_cores)`.
    pub fn load_with_workers(workers: usize) -> Result<Self> {
        let workers = workers.clamp(1, MAX_WORKERS);
        log::debug!("CandleBertModel::load_with_workers: repo={MODEL_REPO} workers={workers}");
        let (config_path, tokenizer_path, weights_path) = download_files()?;

        let config_str = std::fs::read_to_string(&config_path)
            .wrap_err_with(|| format!("failed to read config.json at {}", config_path.display()))?;
        let config: Config =
            serde_json::from_str(&config_str).wrap_err("failed to parse bge-small-en-v1.5 config.json")?;
        if config.hidden_size != DIM {
            eyre::bail!(
                "BGE config.hidden_size = {} (expected {DIM}); model checkpoint drifted",
                config.hidden_size
            );
        }
        let pad_token_id = config.pad_token_id as u32;

        let mut replicas = Vec::with_capacity(workers);
        for _ in 0..workers {
            let inner = build_inner(&tokenizer_path, &weights_path, &config, pad_token_id)?;
            replicas.push(Mutex::new(inner));
        }

        log::info!(
            "CandleBertModel::load_with_workers: loaded {MODEL_REPO} dim={DIM} workers={workers} pad_token_id={pad_token_id}"
        );
        Ok(Self {
            replicas,
            next_replica: AtomicUsize::new(0),
            model_version: CANDLE_MODEL_VERSION.to_string(),
        })
    }

    /// Idempotent prefetch: download the three required files (config,
    /// tokenizer, weights) into the hf-hub cache without instantiating
    /// the model. Used by `cortex embed --prefetch-model`.
    pub fn prefetch_bge_small() -> Result<()> {
        log::debug!("CandleBertModel::prefetch_bge_small: repo={MODEL_REPO}");
        let _ = download_files()?;
        log::info!("CandleBertModel::prefetch_bge_small: cache warmed for {MODEL_REPO}");
        Ok(())
    }

    fn embed_inner(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let n = texts.len();
        log::debug!(
            "CandleBertModel::embed_inner: batch_len={n} replicas={}",
            self.replicas.len()
        );
        if n == 0 {
            return Ok(Vec::new());
        }

        // Tiny-workload guard: single-input (e.g. oracle query path)
        // runs synchronously on one replica. Avoids rayon dispatch
        // overhead and matches the steady-state daemon path where each
        // tick has ~1-2 inputs.
        if n == 1 || self.replicas.len() == 1 {
            let idx = self
                .next_replica
                .fetch_add(1, Ordering::Relaxed)
                .rem_euclid(self.replicas.len());
            return forward_replica(&self.replicas[idx], texts);
        }

        // Split the batch across replicas. `sub_chunk_size` is the size
        // of each rayon work item; with len()=64 and replicas=8 we get
        // 8 chunks of 8 each.
        let sub_chunk_size = n.div_ceil(self.replicas.len()).max(1);
        let chunks: Vec<&[&str]> = texts.chunks(sub_chunk_size).collect();
        let chunk_count = chunks.len();

        // par_iter + enumerate so we can reassemble in input order.
        let mut indexed: Vec<(usize, Result<Vec<Vec<f32>>>)> = chunks
            .par_iter()
            .enumerate()
            .map(|(i, chunk)| {
                // Round-robin replica pick: every chunk index maps to a
                // distinct replica when `chunk_count <= replicas.len()`,
                // which is the common case for our batch sizes.
                let replica_idx = i % self.replicas.len();
                let result = forward_replica(&self.replicas[replica_idx], chunk);
                (i, result)
            })
            .collect();

        indexed.sort_by_key(|(i, _)| *i);
        let mut out = Vec::with_capacity(n);
        for (_, result) in indexed {
            out.extend(result?);
        }
        log::debug!(
            "CandleBertModel::embed_inner: completed batch={n} via {chunk_count} sub-chunks across {} replicas",
            self.replicas.len()
        );
        Ok(out)
    }
}

impl EmbeddingModel for CandleBertModel {
    fn dim(&self) -> usize {
        DIM
    }

    fn model_version(&self) -> &str {
        &self.model_version
    }

    fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        let mut v = self.embed_inner(&[text])?;
        v.pop()
            .ok_or_else(|| eyre::eyre!("CandleBertModel::embed_one returned 0 vectors for 1 input"))
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        self.embed_inner(texts)
    }
}

/// Pick a sensible default replica count for this machine. The cap is
/// `MAX_WORKERS`; the floor is `DEFAULT_WORKERS` (1). We use logical
/// parallelism because Candle's scalar kernels do not saturate a
/// physical core on their own, so over-subscribing slightly is fine.
fn default_worker_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(DEFAULT_WORKERS)
        .clamp(1, MAX_WORKERS)
}

/// Load one replica from the same on-disk artifacts. Cheap relative to
/// the network download; the mmap is page-cache-shared with the other
/// replicas so per-replica RSS overhead is small.
fn build_inner(
    tokenizer_path: &std::path::Path,
    weights_path: &std::path::Path,
    config: &Config,
    pad_token_id: u32,
) -> Result<Inner> {
    let device = Device::Cpu;
    // SAFETY: VarBuilder::from_mmaped_safetensors mmaps the weights
    // file. The safetensors format is self-describing and validated
    // up-front; on a corrupted file we get a clean error, not UB. The
    // mmap stays live for the lifetime of the BertModel; we never
    // mutate the underlying bytes. Loading the same file N times keeps
    // the underlying physical pages shared via the OS page cache.
    let vb = unsafe {
        VarBuilder::from_mmaped_safetensors(&[weights_path], DTYPE, &device)
            .wrap_err("failed to mmap model.safetensors")?
    };
    let model = BertModel::load(vb, config).wrap_err("BertModel::load")?;

    let mut tokenizer =
        Tokenizer::from_file(tokenizer_path).map_err(|e| eyre::eyre!("failed to load tokenizer.json: {e}"))?;
    tokenizer
        .with_truncation(Some(TruncationParams {
            max_length: MAX_SEQ_LEN,
            ..Default::default()
        }))
        .map_err(|e| eyre::eyre!("failed to set truncation params: {e}"))?;
    tokenizer.with_padding(Some(PaddingParams {
        strategy: PaddingStrategy::BatchLongest,
        pad_id: pad_token_id,
        ..Default::default()
    }));

    Ok(Inner {
        model,
        tokenizer,
        device,
        pad_token_id,
    })
}

/// Run inference for one chunk on a chosen replica. Holds the replica's
/// mutex for the full tokenize → forward → pool → normalize → host
/// roundtrip; other replicas remain free for concurrent chunks.
fn forward_replica(replica: &Mutex<Inner>, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
    let mut guard = replica
        .lock()
        .map_err(|_| eyre::eyre!("CandleBertModel replica mutex poisoned"))?;
    let inner = &mut *guard;

    let inputs: Vec<String> = texts.iter().map(|t| (*t).to_string()).collect();
    let encodings = inner
        .tokenizer
        .encode_batch(inputs, true)
        .map_err(|e| eyre::eyre!("tokenizer encode_batch failed: {e}"))?;

    let batch = encodings.len();
    let seq_len = encodings.iter().map(|e| e.get_ids().len()).max().unwrap_or(0);
    if seq_len == 0 {
        return Ok(vec![vec![0.0_f32; DIM]; batch]);
    }

    let mut ids_flat: Vec<u32> = Vec::with_capacity(batch * seq_len);
    let mut types_flat: Vec<u32> = Vec::with_capacity(batch * seq_len);
    let mut mask_flat: Vec<u32> = Vec::with_capacity(batch * seq_len);
    for enc in &encodings {
        ids_flat.extend_from_slice(enc.get_ids());
        types_flat.extend_from_slice(enc.get_type_ids());
        mask_flat.extend_from_slice(enc.get_attention_mask());
    }

    let shape = (batch, seq_len);
    let input_ids = Tensor::from_vec(ids_flat, shape, &inner.device).wrap_err("Tensor::from_vec input_ids")?;
    let token_type_ids =
        Tensor::from_vec(types_flat, shape, &inner.device).wrap_err("Tensor::from_vec token_type_ids")?;
    let attention_mask =
        Tensor::from_vec(mask_flat, shape, &inner.device).wrap_err("Tensor::from_vec attention_mask")?;
    let _ = inner.pad_token_id; // pad_id is baked into the tokenizer above.

    // Output shape: [batch, seq, hidden].
    let hidden = inner
        .model
        .forward(&input_ids, &token_type_ids, Some(&attention_mask))
        .wrap_err("BertModel::forward")?;

    // CLS pool: take token 0 only → [batch, hidden].
    let pooled = hidden.i((.., 0, ..)).wrap_err("CLS pool")?;
    let normalized = l2_normalize(&pooled).wrap_err("L2 normalize")?;

    let host = normalized
        .to_dtype(DType::F32)
        .wrap_err("cast to f32")?
        .to_vec2::<f32>()
        .wrap_err("Tensor::to_vec2")?;
    if host.len() != batch {
        eyre::bail!("forward_replica: host batch={} expected={batch}", host.len());
    }
    if host[0].len() != DIM {
        eyre::bail!("forward_replica: host dim={} expected={DIM}", host[0].len());
    }
    Ok(host)
}

/// Download the three artifacts we need from the HF Hub. Returns the
/// local cache paths; subsequent calls are no-ops (hf-hub validates the
/// cached SHA against the Hub-supplied digest).
fn download_files() -> Result<(PathBuf, PathBuf, PathBuf)> {
    let api = Api::new().wrap_err("hf-hub Api::new")?;
    let repo = api.model(MODEL_REPO.to_string());
    let config = repo.get("config.json").wrap_err("hf-hub fetch config.json")?;
    let tokenizer = repo.get("tokenizer.json").wrap_err("hf-hub fetch tokenizer.json")?;
    let weights = repo
        .get("model.safetensors")
        .wrap_err("hf-hub fetch model.safetensors")?;
    Ok((config, tokenizer, weights))
}

/// L2-normalize the last dimension of a 2-D tensor.
fn l2_normalize(x: &Tensor) -> candle_core::Result<Tensor> {
    let norm = x.sqr()?.sum_keepdim(1)?.sqrt()?;
    x.broadcast_div(&norm)
}

#[cfg(test)]
mod tests;
