//! Local text embedding for hybrid retrieval.
//!
//! Built for the hybrid retrieval design
//! (`docs/design/2026-05-16-hybrid-retrieval-fts5-vector-rrf.md`) and
//! the backend-swap design
//! (`docs/design/2026-05-17-candle-embedding-backend.md`).
//!
//! Two roles share this module:
//!
//! - **Cortex** is the only writer to `note_embeddings`. It loads the model
//!   eagerly at the start of every `cortex embed` invocation because every
//!   call uses inference.
//! - **Oracle** queries `note_embeddings` and embeds the query string on
//!   `mode != bm25` calls. It loads lazily through [`embed_query`] so the
//!   ~1-2 s model-load cost is paid by the first hybrid/vector caller, not
//!   by the MCP startup handshake and not by pure-BM25 callers who never
//!   need embeddings.
//!
//! Both processes pin `bge-small-en-v1.5` (384 dims, L2-normalized output).
//! Two backends compile in: [`candle`] (any x86_64 CPU) and [`onnx`] (ONNX
//! Runtime, AVX2+FMA only, gated by `vault::cpu`). Every load path takes a
//! [`Backend`], resolved from config plus the detected CPU by
//! [`resolve_backend`], and returns a `dyn EmbeddingModel`.
//! The model identifier is stored in the search DB's `embedding_config`
//! table as `active_model`; oracle and cortex read it on every dispatch so
//! the two processes cannot drift onto different models.
//!
//! Model identifiers name the model, never the backend: [`MODELS`] is the one
//! registry both backends read. Candle serves every entry; ONNX serves only an
//! entry whose `onnx_parity` is set, which is what keeps a backend without a
//! parity proof from writing vectors under an id
//! (`docs/design/2026-10-27-cpu-tier-inference-backend.md`).

use eyre::Result;
#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
use std::sync::{Arc, OnceLock, RwLock};

pub mod backend;
#[cfg(feature = "vec-candle")]
pub mod candle;

#[cfg(feature = "vec-onnx")]
pub mod onnx;

#[cfg(feature = "vec-candle")]
pub use candle::CandleBertModel;
#[cfg(feature = "vec-onnx")]
pub use onnx::OnnxEmbedder;

pub use backend::{Backend, BackendChoice, InferenceConfig, resolve_backend, resolve_backend_logged};

#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
use crate::cpu::CpuFeatures;

/// Pin: `bge-small-en-v1.5` outputs 384-dim L2-normalized vectors. Cosine
/// similarity reduces to a plain dot product because both query and stored
/// vectors are unit-length; the brute-force vector scan relies on this.
pub const BGE_SMALL_EN_V15_DIM: usize = 384;

/// Model identifier of `BAAI/bge-small-en-v1.5`, the default model.
pub const BGE_SMALL_MODEL_VERSION: &str = "bge-small-en-v1.5";

/// Model identifier of `BAAI/bge-base-en-v1.5`, the 768-dim A/B candidate.
pub const BGE_BASE_MODEL_VERSION: &str = "bge-base-en-v1.5";

/// One embedding model the backends can serve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelSpec {
    /// Written into `note_embeddings.model_version` and
    /// `embedding_config.active_model`. Names the model, not the backend.
    pub id: &'static str,
    /// Output dimensionality; checked against the checkpoint's `hidden_size`
    /// at load.
    pub dim: usize,
    /// HuggingFace Hub repo both backends download from.
    pub hf_repo: &'static str,
    /// `true` only when an `otto ignored` test proves the ONNX vectors within
    /// 1e-3 cosine of the model's reference fixture
    /// (`vault/tests/regression/onnx/parity.rs`). ONNX refuses an entry
    /// without it, so it can never write vectors under an id it has not
    /// matched.
    pub onnx_parity: bool,
}

/// Every model the registry knows. All entries are standard `BertModel`
/// (CLS-pooled) checkpoints, so adding one is a row here, not a forward-code
/// change. The operator A/Bs a candidate by re-pinning
/// `embedding_config.active_model` (`sb cortex embed --model <id>`, which
/// re-embeds on the change) and measuring with `sb oracle eval`.
///
/// `nomic-embed-text-v2` is deliberately absent: it is a different
/// architecture (`nomic_bert`) and would need its own loader. Pointing
/// `BertModel` at nomic weights would be silently wrong.
pub const MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: BGE_SMALL_MODEL_VERSION,
        dim: BGE_SMALL_EN_V15_DIM,
        hf_repo: "BAAI/bge-small-en-v1.5",
        onnx_parity: true,
    },
    ModelSpec {
        id: BGE_BASE_MODEL_VERSION,
        dim: 768,
        hf_repo: "BAAI/bge-base-en-v1.5",
        onnx_parity: false,
    },
];

/// Look up a model by its identifier; `None` if the registry lacks it.
pub fn model_spec(id: &str) -> Option<&'static ModelSpec> {
    MODELS.iter().find(|spec| spec.id == id)
}

/// The model identifier a fresh search DB pins in
/// `embedding_config.active_model`, and the fallback when no pin is readable.
pub const ACTIVE_MODEL_VERSION: &str = BGE_SMALL_MODEL_VERSION;

/// Port for the embedding model.
///
/// The trait is the seam tests use to inject [`MockEmbedder`] without
/// loading the real ~100 MB model. Production code holds a
/// `dyn EmbeddingModel` from [`load_model_version`] or calls the
/// [`embed_query`] convenience.
///
/// `Send + Sync` so a single instance can be shared across threads
/// (cortex holds one in its daemon; oracle's [`embed_query`] keeps
/// process-local instances behind a `RwLock`).
pub trait EmbeddingModel: Send + Sync {
    /// Dimensionality of the output vectors.
    fn dim(&self) -> usize;

    /// Stable model identifier, e.g. `"bge-small-en-v1.5"` (a [`ModelSpec`]
    /// id). Written into `note_embeddings.model_version`; a model change
    /// makes every row stale, a backend change does not.
    fn model_version(&self) -> &str;

    /// Embed one text. Returns a vector of length `self.dim()`.
    fn embed_one(&self, text: &str) -> Result<Vec<f32>>;

    /// Embed a batch of texts. Returns a vector of vectors, one per input
    /// in the same order. The default batches one-at-a-time; adapters
    /// that can amortize tokenization, GPU launches, or internal worker
    /// pools override this.
    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        texts.iter().map(|t| self.embed_one(t)).collect()
    }
}

#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
type RegistryEntry = (String, Arc<dyn EmbeddingModel>);

/// Process-local registry of loaded models, keyed by `model_version`.
///
/// Oracle dispatches every query embedding through [`embed_query`]; the
/// first call for a given `model_version` resolves the backend, performs the
/// lazy load, stores the model in the registry, and returns. Subsequent calls
/// hit the registry at ~0 ms overhead. Model bumps are rare, so holding
/// multiple models resident is acceptable. The backend is fixed per process
/// (it comes from config and the CPU), so the key needs no backend part.
#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
static MODEL_REGISTRY: OnceLock<RwLock<Vec<RegistryEntry>>> = OnceLock::new();

#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
fn registry() -> &'static RwLock<Vec<RegistryEntry>> {
    MODEL_REGISTRY.get_or_init(|| RwLock::new(Vec::new()))
}

/// Load the default model ([`ACTIVE_MODEL_VERSION`]) on `backend`.
#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
pub fn load_active_model(backend: Backend, threads: usize, cpu: &CpuFeatures) -> Result<Box<dyn EmbeddingModel>> {
    load_model_version(ACTIVE_MODEL_VERSION, backend, threads, cpu)
}

/// Load `model_version` on the resolved `backend` (see [`resolve_backend`]).
///
/// `threads == 0` is the backend's default. Candle reads it as its replica
/// count (`min(8, available_parallelism)` at 0); ONNX as its intra-op thread
/// count ([`onnx::DEFAULT_ONNX_THREADS`] at 0, capped at available
/// parallelism). `cpu` reaches the ONNX loader, which refuses an incapable
/// CPU before any `ort` call.
#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
pub fn load_model_version(
    model_version: &str,
    backend: Backend,
    threads: usize,
    cpu: &CpuFeatures,
) -> Result<Box<dyn EmbeddingModel>> {
    log::debug!("embedding::load_model_version: model={model_version} backend={backend} threads={threads}");
    match backend {
        #[cfg(feature = "vec-candle")]
        Backend::Candle => {
            let _ = cpu;
            Ok(Box::new(candle::CandleBertModel::load_version(model_version, threads)?))
        }
        #[cfg(feature = "vec-onnx")]
        Backend::Onnx => Ok(Box::new(onnx::OnnxEmbedder::load_version(model_version, threads, cpu)?)),
        #[allow(unreachable_patterns)]
        other => eyre::bail!("this binary was built without the {other} embedding backend"),
    }
}

/// Whether `backend` in this binary can load `model_version`. Lets callers
/// validate a requested pin before loading or re-embedding.
#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
pub fn is_supported_model_version(model_version: &str, backend: Backend) -> bool {
    match backend {
        #[cfg(feature = "vec-candle")]
        Backend::Candle => candle::supported_model(model_version).is_some(),
        #[cfg(feature = "vec-onnx")]
        Backend::Onnx => onnx::supported_model(model_version).is_some(),
        #[allow(unreachable_patterns)]
        _ => false,
    }
}

/// Idempotent prefetch of the default model's files for `backend`.
#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
pub fn prefetch_active_model(backend: Backend) -> Result<()> {
    prefetch_model_version(ACTIVE_MODEL_VERSION, backend)
}

/// Download `model_version`'s files for `backend` into the shared hf-hub
/// cache without loading the model (warm a host, or a candidate before
/// re-embedding). Candle fetches `model.safetensors`; ONNX fetches
/// `onnx/model.onnx`; both fetch `config.json` and `tokenizer.json`. Runs on
/// any CPU: no session is built.
#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
pub fn prefetch_model_version(model_version: &str, backend: Backend) -> Result<()> {
    log::debug!("embedding::prefetch_model_version: model={model_version} backend={backend}");
    match backend {
        #[cfg(feature = "vec-candle")]
        Backend::Candle => candle::CandleBertModel::prefetch_version(model_version),
        #[cfg(feature = "vec-onnx")]
        Backend::Onnx => onnx::OnnxEmbedder::prefetch_version(model_version),
        #[allow(unreachable_patterns)]
        other => eyre::bail!("this binary was built without the {other} embedding backend"),
    }
}

/// Lazy-load `model_version` and embed `text`.
///
/// Used by oracle's hybrid/vector dispatch where every query needs an
/// embedding and the model identifier comes from the DB's
/// `embedding_config.active_model`. Cortex does NOT use this; it loads a
/// concrete model at the start of every `cortex embed` invocation because
/// every call uses inference, so the lazy registry adds no value there.
///
/// The first call for a `model_version` resolves `inference.backend` against
/// `cpu` and that model (logging the resolution at INFO), then loads it with
/// `inference.threads`. A model no backend in this binary can serve returns
/// a clear error; this protects against a stale hardcoded `model_version`.
#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
pub fn embed_query(
    text: &str,
    model_version: &str,
    inference: &InferenceConfig,
    cpu: &CpuFeatures,
) -> Result<Vec<f32>> {
    let model = get_or_load_model(model_version, inference, cpu)?;
    model.embed_one(text)
}

#[cfg(any(feature = "vec-candle", feature = "vec-onnx"))]
fn get_or_load_model(
    model_version: &str,
    inference: &InferenceConfig,
    cpu: &CpuFeatures,
) -> Result<Arc<dyn EmbeddingModel>> {
    {
        let guard = registry()
            .read()
            .map_err(|_| eyre::eyre!("MODEL_REGISTRY read lock poisoned"))?;
        if let Some(entry) = guard.iter().find(|(k, _)| k == model_version) {
            return Ok(entry.1.clone());
        }
    }
    if model_spec(model_version).is_none() {
        eyre::bail!(
            "unknown model_version: {model_version:?} (registry serves {:?})",
            MODELS.iter().map(|spec| spec.id).collect::<Vec<_>>()
        );
    }
    let backend = resolve_backend_logged(inference.backend, cpu, model_version)?;
    if !is_supported_model_version(model_version, backend) {
        eyre::bail!("the {backend} backend in this binary cannot load model_version {model_version:?}");
    }
    let model: Arc<dyn EmbeddingModel> = Arc::from(load_model_version(model_version, backend, inference.threads, cpu)?);
    let mut guard = registry()
        .write()
        .map_err(|_| eyre::eyre!("MODEL_REGISTRY write lock poisoned"))?;
    if let Some(entry) = guard.iter().find(|(k, _)| k == model_version) {
        return Ok(entry.1.clone());
    }
    guard.push((model_version.to_string(), model.clone()));
    Ok(model)
}

/// Sliding-window chunker for transcript text.
///
/// From the hybrid retrieval design. Splits `text` into
/// overlapping chunks suitable for embedding. The window is measured
/// in whitespace-separated words, which is a coarse approximation of
/// the BPE token count. For English with the bge-small-en-v1.5
/// tokenizer the word:token ratio is ~0.75, so the default
/// `max_tokens = 400` yields chunks of about 530 BPE tokens - right
/// up against the model's 512-token limit. The active backend truncates
/// gracefully on overshoot; if profiling shows truncation hurts
/// recall, drop `max_tokens` to 300.
///
/// `overlap_tokens` is the size of the trailing window each new chunk
/// shares with the previous chunk. The default of 50 keeps claims
/// that straddle a chunk boundary visible in both chunks.
///
/// Edge cases:
/// - Empty or whitespace-only input: returns `Vec::new()`.
/// - Input shorter than `max_tokens`: returns one chunk containing
///   the whole text (no padding, no trailing whitespace).
/// - A single very long word (no whitespace): returns one chunk
///   containing the full word; truncation falls to the embedder.
/// - `overlap_tokens >= max_tokens`: overlap is clamped to
///   `max_tokens - 1` so the loop always makes forward progress.
pub fn chunk_transcript(text: &str, max_tokens: usize, overlap_tokens: usize) -> Vec<String> {
    if max_tokens == 0 {
        return Vec::new();
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return Vec::new();
    }
    if words.len() <= max_tokens {
        return vec![words.join(" ")];
    }

    let overlap = overlap_tokens.min(max_tokens.saturating_sub(1));
    let stride = max_tokens - overlap;
    let mut chunks = Vec::new();
    let mut start = 0;
    while start < words.len() {
        let end = (start + max_tokens).min(words.len());
        let chunk = words[start..end].join(" ");
        chunks.push(chunk);
        if end == words.len() {
            break;
        }
        start += stride;
    }
    chunks
}

#[cfg(any(test, feature = "test-util"))]
/// Deterministic test embedder.
///
/// Produces vectors derived from a 64-bit hash of the input text. The
/// hash is folded into the requested dimension and L2-normalized so the
/// output is compatible with cosine-similarity scoring (dot product on
/// unit vectors). Two different inputs produce different vectors; the
/// same input is stable across calls and across processes.
///
/// Used by tests that need a real `EmbeddingModel` without the
/// ~1-2 s real-model load cost.
pub struct MockEmbedder {
    dim: usize,
    model_version: String,
}

#[cfg(any(test, feature = "test-util"))]
impl MockEmbedder {
    /// Build a mock embedder with the given dimension and version label.
    pub fn new(dim: usize, model_version: impl Into<String>) -> Self {
        Self {
            dim,
            model_version: model_version.into(),
        }
    }

    /// Build a mock embedder at the default 384 dim with a stable test label.
    pub fn default_384() -> Self {
        Self::new(BGE_SMALL_EN_V15_DIM, "mock-bge-small-en-v1.5")
    }
}

#[cfg(any(test, feature = "test-util"))]
impl EmbeddingModel for MockEmbedder {
    fn dim(&self) -> usize {
        self.dim
    }

    fn model_version(&self) -> &str {
        &self.model_version
    }

    fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        let seed = hash64(text);
        let mut state = seed;
        let mut out = Vec::with_capacity(self.dim);
        for _ in 0..self.dim {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let bits = (state >> 32) as u32;
            let v = (bits as f32 / u32::MAX as f32) - 0.5;
            out.push(v);
        }
        l2_normalize(&mut out);
        Ok(out)
    }
}

#[cfg(any(test, feature = "test-util"))]
/// 64-bit FNV-1a hash. Stable across releases of Rust because it has no
/// dependency on the standard library's hashing implementation. Used by
/// [`MockEmbedder`] to derive deterministic seeds from input text.
fn hash64(text: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[cfg(any(test, feature = "test-util"))]
/// L2-normalize a vector in place. Zero-length vectors are left unchanged
/// (their cosine similarity to anything is undefined anyway).
fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > f32::EPSILON {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

#[cfg(test)]
mod tests;
