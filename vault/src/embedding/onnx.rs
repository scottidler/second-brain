//! ONNX Runtime embedding adapter (gated behind `vec-onnx`).
//!
//! Implements `EmbeddingModel` for `BAAI/bge-small-en-v1.5` by running the
//! repo's own `onnx/model.onnx` export through `ort`, with the same
//! `tokenizer.json`, truncation (512), CLS pool and L2 norm as the Candle
//! backend. `docs/design/2026-10-27-cpu-tier-inference-backend.md`.
//!
//! - **CPU gate.** [`OnnxEmbedder::load_version`] refuses a CPU without
//!   AVX2+FMA before any `ort` call (see `vault::onnx`). Callers pass the
//!   [`CpuFeatures`] they detected, so tests can inject one.
//! - **Threads.** One `Session` with `intra_threads = N`. ORT's thread pool is
//!   the parallelism, so there are no replicas: `Session::run` takes `&mut
//!   self` in `ort` rc.15, and the session sits behind a `Mutex`.
//! - **Parity.** A model id is listed here only when an `otto ignored` test
//!   proves its vectors within 1e-3 cosine of the sentence-transformers
//!   reference (`vault/tests/regression/onnx/parity.rs`). Today that is
//!   `bge-small-en-v1.5` alone.

use eyre::{Result, WrapErr, eyre};
use hf_hub::api::sync::Api;
use ort::session::Session;
use ort::value::Tensor;
use std::path::PathBuf;
use std::sync::Mutex;
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer, TruncationParams};

use super::EmbeddingModel;
use crate::cpu::CpuFeatures;
use crate::onnx::{build_session, require_onnx_capable};

/// Model identifier this backend serves for `BAAI/bge-small-en-v1.5`.
///
/// Backend-free on purpose: the ONNX vectors match the Candle ones to 1e-13
/// (measured on `ripr`), so the id names the model, not the runtime.
pub const ONNX_MODEL_VERSION: &str = "bge-small-en-v1.5";

/// Intra-op threads when the caller asks for the default (`threads == 0`).
///
/// The measured knee on `ripr` (Threadripper 9970X, 64 threads), Rust ORT,
/// 512-token chunks, batch 16; numbers and the rule in the design doc's
/// "Measured" section. Capped at the host's available parallelism.
pub const DEFAULT_ONNX_THREADS: usize = 16;

/// Hard token limit declared by the bge-small-en-v1.5 model card.
const MAX_SEQ_LEN: usize = 512;

/// Path of the ONNX export inside each HF repo.
const MODEL_FILE: &str = "onnx/model.onnx";

/// Models with an ONNX parity proof: `(model_version, hf_repo, dim)`.
const SUPPORTED_MODELS: &[(&str, &str, usize)] = &[(ONNX_MODEL_VERSION, "BAAI/bge-small-en-v1.5", 384)];

/// Resolve a model version to its `(hf_repo, dim)`; `None` if this backend
/// has no parity proof for it.
pub fn supported_model(version: &str) -> Option<(&'static str, usize)> {
    SUPPORTED_MODELS
        .iter()
        .find(|(v, _, _)| *v == version)
        .map(|(_, repo, dim)| (*repo, *dim))
}

/// The model versions this backend can serve (for error messages).
pub fn supported_versions() -> Vec<&'static str> {
    SUPPORTED_MODELS.iter().map(|(v, _, _)| *v).collect()
}

/// ONNX Runtime BERT encoder producing L2-normalized CLS embeddings.
pub struct OnnxEmbedder {
    session: Mutex<Session>,
    tokenizer: Tokenizer,
    feeds_token_type_ids: bool,
    model_version: String,
    dim: usize,
}

impl OnnxEmbedder {
    /// Download (if needed) and load `version` with `threads` intra-op
    /// threads (`0` = [`default_thread_count`]).
    ///
    /// Returns `Err` before any download or `ort` call when `cpu` lacks AVX2
    /// or FMA, or when `version` has no ONNX parity proof.
    pub fn load_version(version: &str, threads: usize, cpu: &CpuFeatures) -> Result<Self> {
        log::debug!("OnnxEmbedder::load_version: version={version} threads={threads}");
        require_onnx_capable(cpu)?;
        let (repo, dim) = lookup(version)?;
        let threads = resolve_threads(threads);
        let files = download_files(repo)?;

        let config = read_config(&files.config, repo)?;
        if config.hidden_size != dim {
            eyre::bail!(
                "{repo} config.hidden_size = {} (expected {dim} for {version}); model checkpoint drifted",
                config.hidden_size
            );
        }
        let tokenizer = load_tokenizer(&files.tokenizer, config.pad_token_id)?;
        let session = build_session(&files.model, threads, cpu)?;

        let input_names: Vec<&str> = session.inputs().iter().map(|i| i.name()).collect();
        for required in ["input_ids", "attention_mask"] {
            if !input_names.contains(&required) {
                eyre::bail!("{repo} {MODEL_FILE} has no {required:?} input (inputs: {input_names:?})");
            }
        }
        let feeds_token_type_ids = input_names.contains(&"token_type_ids");

        log::info!(
            "OnnxEmbedder::load_version: loaded {repo} version={version} dim={dim} threads={threads} \
             token_type_ids={feeds_token_type_ids}"
        );
        Ok(Self {
            session: Mutex::new(session),
            tokenizer,
            feeds_token_type_ids,
            model_version: version.to_string(),
            dim,
        })
    }

    /// Idempotent prefetch of `version`'s ONNX files (config, tokenizer,
    /// `onnx/model.onnx`) into the shared hf-hub cache, without building a
    /// session (so it runs on any CPU).
    pub fn prefetch_version(version: &str) -> Result<()> {
        log::debug!("OnnxEmbedder::prefetch_version: version={version}");
        let (repo, _dim) = lookup(version)?;
        download_files(repo)?;
        log::info!("OnnxEmbedder::prefetch_version: cache warmed for {repo}");
        Ok(())
    }

    fn embed_inner(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let batch = texts.len();
        log::debug!("OnnxEmbedder::embed_inner: batch_len={batch}");
        if batch == 0 {
            return Ok(Vec::new());
        }
        let encodings = self
            .tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| eyre!("tokenizer encode_batch failed: {e}"))?;
        let seq_len = encodings.iter().map(|e| e.get_ids().len()).max().unwrap_or(0);
        if let Some(bad) = encodings.iter().find(|e| e.get_ids().len() != seq_len) {
            eyre::bail!(
                "tokenizer padding left a ragged batch: {} vs {seq_len} tokens",
                bad.get_ids().len()
            );
        }
        if seq_len == 0 {
            return Ok(vec![vec![0.0_f32; self.dim]; batch]);
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
            .map_err(|_| eyre!("OnnxEmbedder session mutex poisoned"))?;
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
        let (out_shape, hidden) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| eyre!("extract last_hidden_state: {e}"))?;
        cls_pool_normalize(out_shape, hidden, batch, self.dim)
    }
}

impl EmbeddingModel for OnnxEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn model_version(&self) -> &str {
        &self.model_version
    }

    fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        let mut v = self.embed_inner(&[text])?;
        v.pop()
            .ok_or_else(|| eyre!("OnnxEmbedder::embed_one returned 0 vectors for 1 input"))
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        self.embed_inner(texts)
    }
}

/// The default intra-op thread count on this host:
/// `min(DEFAULT_ONNX_THREADS, available_parallelism)`.
pub fn default_thread_count() -> usize {
    DEFAULT_ONNX_THREADS.min(available_parallelism())
}

/// `0` means [`default_thread_count`]; any other request is capped at the
/// host's available parallelism, since ORT intra-op threads past the logical
/// CPU count only contend.
fn resolve_threads(requested: usize) -> usize {
    if requested == 0 {
        default_thread_count()
    } else {
        requested.min(available_parallelism())
    }
}

fn available_parallelism() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

fn lookup(version: &str) -> Result<(&'static str, usize)> {
    supported_model(version).ok_or_else(|| {
        eyre!(
            "no ONNX parity proof for model version {version:?}; ONNX serves: {:?}",
            supported_versions()
        )
    })
}

/// Take token 0 of each sequence from a `[batch, seq, dim]` hidden-state
/// tensor and L2-normalize it.
fn cls_pool_normalize(shape: &[i64], hidden: &[f32], batch: usize, dim: usize) -> Result<Vec<Vec<f32>>> {
    let [b, seq, d] = shape else {
        eyre::bail!(
            "ONNX output rank {} (shape {shape:?}), expected [batch, seq, {dim}]",
            shape.len()
        );
    };
    let (b, seq, d) = (usize::try_from(*b)?, usize::try_from(*seq)?, usize::try_from(*d)?);
    if b != batch || d != dim || seq == 0 {
        eyre::bail!("ONNX output shape {shape:?}, expected [{batch}, >0, {dim}]");
    }
    if hidden.len() != b * seq * d {
        eyre::bail!("ONNX output holds {} floats for shape {shape:?}", hidden.len());
    }
    Ok(hidden
        .chunks_exact(seq * d)
        .map(|sequence| {
            let cls = &sequence[..d];
            let norm = cls.iter().map(|x| x * x).sum::<f32>().sqrt().max(f32::MIN_POSITIVE);
            cls.iter().map(|x| x / norm).collect()
        })
        .collect())
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
    hidden_size: usize,
    #[serde(default)]
    pad_token_id: u32,
}

fn read_config(path: &std::path::Path, repo: &str) -> Result<BertConfig> {
    let raw =
        std::fs::read_to_string(path).wrap_err_with(|| format!("failed to read config.json at {}", path.display()))?;
    serde_json::from_str(&raw).wrap_err_with(|| format!("failed to parse {repo} config.json"))
}

fn load_tokenizer(path: &std::path::Path, pad_id: u32) -> Result<Tokenizer> {
    let mut tokenizer = Tokenizer::from_file(path).map_err(|e| eyre!("failed to load tokenizer.json: {e}"))?;
    tokenizer
        .with_truncation(Some(TruncationParams {
            max_length: MAX_SEQ_LEN,
            ..Default::default()
        }))
        .map_err(|e| eyre!("failed to set truncation params: {e}"))?;
    tokenizer.with_padding(Some(PaddingParams {
        strategy: PaddingStrategy::BatchLongest,
        pad_id,
        ..Default::default()
    }));
    Ok(tokenizer)
}

#[cfg(test)]
mod tests;
