//! Candle-backed cross-encoder reranker (feature `vec-candle`). See the parent
//! `rerank.rs` for the contract and the honest on-host status.
//!
//! Architecture (`BertForSequenceClassification`, built from parts because
//! candle-transformers exposes only `BertModel`):
//!
//! ```text
//! (query, doc) -> tokenizer pair -> BertModel -> CLS token
//!   -> [optional pooler: Linear + tanh] -> classifier: Linear(hidden -> 1)
//!   -> single relevance logit
//! ```
//!
//! No L2-normalization (unlike the bi-encoder embedder): the classifier logit
//! IS the score. Higher = more relevant.

use candle_core::{DType, Device, IndexOp, Tensor};
use candle_nn::{Linear, Module, VarBuilder};
use candle_transformers::models::bert::{BertModel, Config, DTYPE};
use eyre::{Result, WrapErr};
use hf_hub::api::sync::Api;
use std::path::PathBuf;
use std::sync::Mutex;
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

use super::{Reranker, cross_encoder};

/// Hard token cap for a (query, doc) pair.
const MAX_SEQ_LEN: usize = 512;

/// Candle cross-encoder for a `super::CROSS_ENCODERS` entry. A single inner model
/// behind a `Mutex` (Candle's tensor pipeline is not concurrency-safe on one
/// instance); a `score` call batches all pairs into one padded forward.
pub struct CandleCrossEncoder {
    inner: Mutex<Inner>,
    model_id: String,
}

struct Inner {
    model: BertModel,
    pooler: Option<Linear>,
    classifier: Linear,
    tokenizer: Tokenizer,
    device: Device,
}

impl CandleCrossEncoder {
    /// Download (if needed) and load the cross-encoder. Candle is the
    /// reference backend and serves every registry entry; an id outside the
    /// registry is a clear error (no silent wrong model).
    pub fn load(model_id: &str) -> Result<Self> {
        let repo = cross_encoder(model_id)?.hf_repo;
        log::debug!("CandleCrossEncoder::load: repo={repo}");
        let (config_path, tokenizer_path, weights_path) = download_files(repo)?;

        let config_str = std::fs::read_to_string(&config_path).wrap_err("read cross-encoder config.json")?;
        let config: Config = serde_json::from_str(&config_str).wrap_err("parse cross-encoder config.json")?;
        let hidden = config.hidden_size;
        let device = Device::Cpu;

        // SAFETY: mirrors the embedder's mmap load (self-describing, validated
        // up-front; mapped bytes live for the model's lifetime and are never
        // mutated).
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[weights_path], DTYPE, &device)
                .wrap_err("mmap cross-encoder model.safetensors")?
        };

        // BertForSequenceClassification stores the encoder under `bert.`; some
        // exports keep it at the root. Detect by probing a known tensor.
        let encoder_vb = if vb.pp("bert").contains_tensor("embeddings.word_embeddings.weight") {
            vb.pp("bert")
        } else {
            vb.clone()
        };
        let model = BertModel::load(encoder_vb.clone(), &config).wrap_err("BertModel::load (cross-encoder)")?;

        // Optional BERT pooler (dense + tanh on CLS). Present in standard
        // BertForSequenceClassification; absent in some distilled exports.
        let pooler_vb = encoder_vb.pp("pooler").pp("dense");
        let pooler = if pooler_vb.contains_tensor("weight") {
            Some(candle_nn::linear(hidden, hidden, pooler_vb).wrap_err("load pooler dense")?)
        } else {
            None
        };

        // Classification head: Linear(hidden -> 1). ms-marco cross-encoders are
        // single-logit regressors.
        let classifier = candle_nn::linear(hidden, 1, vb.pp("classifier")).wrap_err("load classifier head")?;

        let mut tokenizer =
            Tokenizer::from_file(&tokenizer_path).map_err(|e| eyre::eyre!("load cross-encoder tokenizer.json: {e}"))?;
        tokenizer
            .with_truncation(Some(TruncationParams {
                max_length: MAX_SEQ_LEN,
                ..Default::default()
            }))
            .map_err(|e| eyre::eyre!("set truncation: {e}"))?;
        tokenizer.with_padding(Some(PaddingParams {
            strategy: PaddingStrategy::BatchLongest,
            pad_id: config.pad_token_id as u32,
            ..Default::default()
        }));

        log::info!(
            "CandleCrossEncoder::load: loaded {repo} hidden={hidden} pooler={}",
            pooler.is_some()
        );
        Ok(Self {
            inner: Mutex::new(Inner {
                model,
                pooler,
                classifier,
                tokenizer,
                device,
            }),
            model_id: model_id.to_string(),
        })
    }

    /// Idempotent prefetch of `model_id`'s safetensors files into the hf-hub
    /// cache, without loading the model.
    pub fn prefetch(model_id: &str) -> Result<()> {
        download_files(cross_encoder(model_id)?.hf_repo).map(|_| ())
    }
}

impl Reranker for CandleCrossEncoder {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn score(&self, query: &str, docs: &[&str]) -> Result<Vec<f32>> {
        if docs.is_empty() {
            return Ok(Vec::new());
        }
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| eyre::eyre!("CandleCrossEncoder mutex poisoned"))?;
        let inner = &mut *guard;

        // (query, doc) pairs -> BERT sequence pairs with segment ids.
        let pairs: Vec<(String, String)> = docs.iter().map(|d| (query.to_string(), (*d).to_string())).collect();
        let encodings = inner
            .tokenizer
            .encode_batch(pairs, true)
            .map_err(|e| eyre::eyre!("tokenizer encode_batch (pairs): {e}"))?;

        let batch = encodings.len();
        let seq_len = encodings.iter().map(|e| e.get_ids().len()).max().unwrap_or(0);
        if seq_len == 0 {
            return Ok(vec![0.0_f32; batch]);
        }

        let mut ids: Vec<u32> = Vec::with_capacity(batch * seq_len);
        let mut types: Vec<u32> = Vec::with_capacity(batch * seq_len);
        let mut mask: Vec<u32> = Vec::with_capacity(batch * seq_len);
        for enc in &encodings {
            ids.extend_from_slice(enc.get_ids());
            types.extend_from_slice(enc.get_type_ids());
            mask.extend_from_slice(enc.get_attention_mask());
        }

        let shape = (batch, seq_len);
        let input_ids = Tensor::from_vec(ids, shape, &inner.device).wrap_err("Tensor::from_vec input_ids")?;
        let token_type_ids =
            Tensor::from_vec(types, shape, &inner.device).wrap_err("Tensor::from_vec token_type_ids")?;
        let attention_mask =
            Tensor::from_vec(mask, shape, &inner.device).wrap_err("Tensor::from_vec attention_mask")?;

        let hidden = inner
            .model
            .forward(&input_ids, &token_type_ids, Some(&attention_mask))
            .wrap_err("BertModel::forward")?;
        // CLS token -> [batch, hidden].
        let cls = hidden.i((.., 0, ..)).wrap_err("CLS slice")?;
        let pooled = match &inner.pooler {
            Some(p) => p
                .forward(&cls)
                .wrap_err("pooler dense")?
                .tanh()
                .wrap_err("pooler tanh")?,
            None => cls,
        };
        // classifier -> [batch, 1] -> [batch].
        let logits = inner.classifier.forward(&pooled).wrap_err("classifier")?;
        let logits = logits.squeeze(1).wrap_err("squeeze logits")?;
        let scores = logits
            .to_dtype(DType::F32)
            .wrap_err("cast logits to f32")?
            .to_vec1::<f32>()
            .wrap_err("logits to_vec1")?;
        if scores.len() != batch {
            eyre::bail!("cross-encoder produced {} scores for {batch} pairs", scores.len());
        }
        Ok(scores)
    }
}

fn download_files(repo_id: &str) -> Result<(PathBuf, PathBuf, PathBuf)> {
    let api = Api::new().wrap_err("hf-hub Api::new")?;
    let repo = api.model(repo_id.to_string());
    let config = repo.get("config.json").wrap_err("fetch cross-encoder config.json")?;
    let tokenizer = repo
        .get("tokenizer.json")
        .wrap_err("fetch cross-encoder tokenizer.json")?;
    let weights = repo
        .get("model.safetensors")
        .wrap_err("fetch cross-encoder model.safetensors")?;
    Ok((config, tokenizer, weights))
}
