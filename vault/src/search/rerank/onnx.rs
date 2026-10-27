//! ONNX Runtime cross-encoder reranker (feature `vec-onnx`). See the parent
//! `rerank.rs` for the contract.
//!
//! Runs the HF repo's own `onnx/model.onnx` export of
//! `BertForSequenceClassification` (encoder, pooler and classifier head in
//! one graph), so the single output is the relevance logit, `[batch, 1]`.
//! Same `tokenizer.json`, pair encoding, 512-token truncation and padding as
//! [`super::CandleCrossEncoder`].
//!
//! - **CPU gate.** [`OnnxCrossEncoder::load`] refuses a CPU without AVX2+FMA
//!   before any download or `ort` call; the session is built through
//!   `vault::onnx::build_session`, which owns the process's one ORT
//!   environment and checks again.
//! - **Threads.** One `Session` with `intra_threads = N` behind a `Mutex`
//!   (`Session::run` takes `&mut self`); `0` is the embedder's default.

use eyre::{Result, WrapErr, eyre};
use hf_hub::api::sync::Api;
use ort::session::Session;
use ort::value::Tensor;
use std::path::PathBuf;
use std::sync::Mutex;
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

use super::{CrossEncoderSpec, Reranker, onnx_cross_encoder};
use crate::cpu::CpuFeatures;
use crate::embedding::onnx::resolve_threads;
use crate::onnx::{build_session, require_onnx_capable};

/// Hard token cap for a `(query, doc)` pair.
const MAX_SEQ_LEN: usize = 512;

/// Path of the ONNX export inside the HF repo.
const MODEL_FILE: &str = "onnx/model.onnx";

/// ONNX Runtime cross-encoder: one relevance logit per `(query, doc)` pair.
pub struct OnnxCrossEncoder {
    session: Mutex<Session>,
    tokenizer: Tokenizer,
    feeds_token_type_ids: bool,
    model_id: String,
}

impl OnnxCrossEncoder {
    /// Download (if needed) and load `model_id` with `threads` intra-op
    /// threads (`0` = the ONNX default).
    ///
    /// Returns `Err` before any download or `ort` call when `cpu` lacks AVX2
    /// or FMA, or when `model_id` has no ONNX parity proof.
    pub fn load(model_id: &str, threads: usize, cpu: &CpuFeatures) -> Result<Self> {
        log::debug!("OnnxCrossEncoder::load: model_id={model_id} threads={threads}");
        require_onnx_capable(cpu)?;
        let CrossEncoderSpec { hf_repo: repo, .. } = *onnx_cross_encoder(model_id)?;
        let threads = resolve_threads(threads);
        let files = download_files(repo)?;

        let pad_id = read_pad_token_id(&files.config, repo)?;
        let tokenizer = load_tokenizer(&files.tokenizer, pad_id)?;
        let session = build_session(&files.model, threads, cpu)?;

        let input_names: Vec<&str> = session.inputs().iter().map(|i| i.name()).collect();
        for required in ["input_ids", "attention_mask"] {
            if !input_names.contains(&required) {
                eyre::bail!("{repo} {MODEL_FILE} has no {required:?} input (inputs: {input_names:?})");
            }
        }
        let feeds_token_type_ids = input_names.contains(&"token_type_ids");

        log::info!(
            "OnnxCrossEncoder::load: loaded {repo} model_id={model_id} threads={threads} \
             token_type_ids={feeds_token_type_ids}"
        );
        Ok(Self {
            session: Mutex::new(session),
            tokenizer,
            feeds_token_type_ids,
            model_id: model_id.to_string(),
        })
    }

    /// Idempotent prefetch of `model_id`'s ONNX files into the shared hf-hub
    /// cache, without building a session (so it runs on any CPU).
    pub fn prefetch(model_id: &str) -> Result<()> {
        log::debug!("OnnxCrossEncoder::prefetch: model_id={model_id}");
        let repo = onnx_cross_encoder(model_id)?.hf_repo;
        download_files(repo)?;
        log::info!("OnnxCrossEncoder::prefetch: cache warmed for {repo}");
        Ok(())
    }
}

impl Reranker for OnnxCrossEncoder {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn score(&self, query: &str, docs: &[&str]) -> Result<Vec<f32>> {
        let batch = docs.len();
        log::debug!("OnnxCrossEncoder::score: batch_len={batch}");
        if batch == 0 {
            return Ok(Vec::new());
        }
        let pairs: Vec<(String, String)> = docs.iter().map(|d| (query.to_string(), (*d).to_string())).collect();
        let encodings = self
            .tokenizer
            .encode_batch(pairs, true)
            .map_err(|e| eyre!("tokenizer encode_batch (pairs): {e}"))?;
        let seq_len = encodings.iter().map(|e| e.get_ids().len()).max().unwrap_or(0);
        if let Some(bad) = encodings.iter().find(|e| e.get_ids().len() != seq_len) {
            eyre::bail!(
                "tokenizer padding left a ragged batch: {} vs {seq_len} tokens",
                bad.get_ids().len()
            );
        }
        if seq_len == 0 {
            return Ok(vec![0.0_f32; batch]);
        }

        let mut ids = Vec::with_capacity(batch * seq_len);
        let mut mask = Vec::with_capacity(batch * seq_len);
        let mut types = Vec::with_capacity(batch * seq_len);
        for enc in &encodings {
            ids.extend(enc.get_ids().iter().map(|&x| i64::from(x)));
            mask.extend(enc.get_attention_mask().iter().map(|&x| i64::from(x)));
            types.extend(enc.get_type_ids().iter().map(|&x| i64::from(x)));
        }
        let shape = [batch, seq_len];
        let ids = Tensor::from_array((shape, ids)).map_err(|e| eyre!("input_ids tensor: {e}"))?;
        let mask = Tensor::from_array((shape, mask)).map_err(|e| eyre!("attention_mask tensor: {e}"))?;

        let mut session = self
            .session
            .lock()
            .map_err(|_| eyre!("OnnxCrossEncoder session mutex poisoned"))?;
        let outputs = if self.feeds_token_type_ids {
            let types = Tensor::from_array((shape, types)).map_err(|e| eyre!("token_type_ids tensor: {e}"))?;
            session.run(ort::inputs![
                "input_ids" => ids,
                "attention_mask" => mask,
                "token_type_ids" => types,
            ])
        } else {
            session.run(ort::inputs!["input_ids" => ids, "attention_mask" => mask])
        }
        .map_err(|e| eyre!("ort run: {e}"))?;
        let (out_shape, logits) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| eyre!("extract logits: {e}"))?;
        single_logits(out_shape, logits, batch)
    }
}

/// The per-pair logits from a `[batch, 1]` classifier output. Any other shape
/// is a model that is not a single-logit cross-encoder, and fails loud.
fn single_logits(shape: &[i64], logits: &[f32], batch: usize) -> Result<Vec<f32>> {
    let [b, labels] = shape else {
        eyre::bail!(
            "cross-encoder output rank {} (shape {shape:?}), expected [batch, 1]",
            shape.len()
        );
    };
    if usize::try_from(*b)? != batch || *labels != 1 {
        eyre::bail!("cross-encoder output shape {shape:?}, expected [{batch}, 1]");
    }
    if logits.len() != batch {
        eyre::bail!("cross-encoder output holds {} floats for shape {shape:?}", logits.len());
    }
    Ok(logits.to_vec())
}

struct ModelFiles {
    config: PathBuf,
    tokenizer: PathBuf,
    model: PathBuf,
}

/// Fetch the three artifacts into the hf-hub cache Candle shares
/// (`~/.cache/huggingface/hub`); cached files are not re-downloaded.
fn download_files(repo_id: &str) -> Result<ModelFiles> {
    let api = Api::new().wrap_err("hf-hub Api::new")?;
    let repo = api.model(repo_id.to_string());
    Ok(ModelFiles {
        config: repo.get("config.json").wrap_err("hf-hub fetch config.json")?,
        tokenizer: repo.get("tokenizer.json").wrap_err("hf-hub fetch tokenizer.json")?,
        model: repo
            .get(MODEL_FILE)
            .wrap_err_with(|| format!("hf-hub fetch {MODEL_FILE}"))?,
    })
}

#[derive(serde::Deserialize)]
struct BertConfig {
    #[serde(default)]
    pad_token_id: u32,
}

fn read_pad_token_id(path: &std::path::Path, repo: &str) -> Result<u32> {
    let raw =
        std::fs::read_to_string(path).wrap_err_with(|| format!("failed to read config.json at {}", path.display()))?;
    let config: BertConfig =
        serde_json::from_str(&raw).wrap_err_with(|| format!("failed to parse {repo} config.json"))?;
    Ok(config.pad_token_id)
}

fn load_tokenizer(path: &std::path::Path, pad_id: u32) -> Result<Tokenizer> {
    let mut tokenizer = Tokenizer::from_file(path).map_err(|e| eyre!("load cross-encoder tokenizer.json: {e}"))?;
    tokenizer
        .with_truncation(Some(TruncationParams {
            max_length: MAX_SEQ_LEN,
            ..Default::default()
        }))
        .map_err(|e| eyre!("set truncation: {e}"))?;
    tokenizer.with_padding(Some(PaddingParams {
        strategy: PaddingStrategy::BatchLongest,
        pad_id,
        ..Default::default()
    }));
    Ok(tokenizer)
}

#[cfg(test)]
mod tests;
