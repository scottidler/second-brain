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
use std::path::PathBuf;
use std::sync::Mutex;
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

/// Candle-backed BertModel that produces `bge-small-en-v1.5` embeddings.
///
/// `BertModel::forward` takes `&self`, but Candle's tensor pipeline
/// shares mutable internal buffers across calls (notably the encoder's
/// attention-mask broadcasts), so it is not safe to call concurrently
/// from multiple threads on the same instance. The `Mutex<Inner>`
/// serialises calls; the lock window is bounded by one inference (~150-
/// 300 ms on scalar CPU, ~5-50 ms on AVX2/MKL). Phase 6 introduces a
/// `Vec<Mutex<Inner>>` replica pool so a single `embed_batch` call can
/// fan out across replicas; for Phase 2 we ship the single-mutex shape.
pub struct CandleBertModel {
    inner: Mutex<Inner>,
    model_version: String,
}

struct Inner {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
    pad_token_id: u32,
}

impl CandleBertModel {
    /// Download (if needed) and load `BAAI/bge-small-en-v1.5` from the
    /// HuggingFace Hub. Cache lives under hf-hub's default location
    /// (`~/.cache/huggingface/hub/`) so it survives binary upgrades.
    pub fn load() -> Result<Self> {
        log::debug!("CandleBertModel::load: repo={MODEL_REPO}");
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

        let device = Device::Cpu;
        // SAFETY: VarBuilder::from_mmaped_safetensors mmaps the weights
        // file. The safetensors format is self-describing and validated
        // up-front; on a corrupted file we get a clean error, not UB.
        // The mmap stays live for the lifetime of the BertModel; we
        // never mutate the underlying bytes.
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[weights_path.as_path()], DTYPE, &device)
                .wrap_err("failed to mmap model.safetensors")?
        };
        let model = BertModel::load(vb, &config).wrap_err("BertModel::load")?;

        let mut tokenizer =
            Tokenizer::from_file(&tokenizer_path).map_err(|e| eyre::eyre!("failed to load tokenizer.json: {e}"))?;
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

        log::info!("CandleBertModel::load: loaded {MODEL_REPO} dim={DIM} pad_token_id={pad_token_id}");
        Ok(Self {
            inner: Mutex::new(Inner {
                model,
                tokenizer,
                device,
                pad_token_id,
            }),
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
        log::debug!("CandleBertModel::embed_inner: batch_len={}", texts.len());
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| eyre::eyre!("CandleBertModel mutex poisoned"))?;
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

        let mut ids_flat = Vec::with_capacity(batch * seq_len);
        let mut types_flat = Vec::with_capacity(batch * seq_len);
        let mut mask_flat = Vec::with_capacity(batch * seq_len);
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

        // Materialise to host-side Vec<Vec<f32>>.
        let host = normalized
            .to_dtype(DType::F32)
            .wrap_err("cast to f32")?
            .to_vec2::<f32>()
            .wrap_err("Tensor::to_vec2")?;
        if host.len() != batch {
            eyre::bail!("embed_inner: host batch={} expected={batch}", host.len());
        }
        if host[0].len() != DIM {
            eyre::bail!("embed_inner: host dim={} expected={DIM}", host[0].len());
        }
        log::debug!("CandleBertModel::embed_inner: returned batch={batch} dim={DIM}");
        Ok(host)
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
