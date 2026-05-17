//! Local text embedding via fastembed's ONNX runtime.
//!
//! Phase A2 of the hybrid retrieval design (`docs/design/2026-05-16-hybrid-retrieval-fts5-vector-rrf.md`).
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
//! The model identifier is stored in the search DB's `embedding_config`
//! table as `active_model`; oracle and cortex read it on every dispatch so
//! the two processes cannot drift onto different models.

use eyre::{Result, WrapErr};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

/// Pin: `bge-small-en-v1.5` outputs 384-dim L2-normalized vectors. Cosine
/// similarity reduces to a plain dot product because both query and stored
/// vectors are unit-length; Phase A3's brute-force scan relies on this.
pub const BGE_SMALL_EN_V15_DIM: usize = 384;

/// Canonical model identifier written into `note_embeddings.model_version`
/// and `embedding_config.active_model`. Oracle and cortex compare against
/// this value when deciding which rows to read or refresh.
pub const BGE_SMALL_EN_V15_NAME: &str = "bge-small-en-v1.5";

/// Port for the embedding model.
///
/// The trait is the seam tests use to inject [`MockEmbedder`] without
/// loading the real ~100 MB ONNX model. Production code calls the trait
/// through [`FastEmbedModel`] or the [`embed_query`] convenience.
///
/// `Send + Sync` so a single instance can be shared across threads (cortex
/// holds one in its daemon; oracle's [`embed_query`] keeps process-local
/// instances behind a `RwLock`).
pub trait EmbeddingModel: Send + Sync {
    /// Dimensionality of the output vectors.
    fn dim(&self) -> usize;

    /// Stable model identifier, e.g. `"bge-small-en-v1.5"`. Written into
    /// `note_embeddings.model_version` so a future model-version bump
    /// can co-exist with old rows.
    fn model_version(&self) -> &str;

    /// Embed one text. Returns a vector of length `self.dim()`.
    fn embed_one(&self, text: &str) -> Result<Vec<f32>>;

    /// Embed a batch of texts. Returns a vector of vectors, one per input
    /// in the same order. The default batches one-at-a-time; adapters
    /// that can amortize tokenization or GPU launches override this.
    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        texts.iter().map(|t| self.embed_one(t)).collect()
    }
}

/// fastembed-backed adapter that pins `bge-small-en-v1.5`.
///
/// Loading the model is expensive (~100 MB cache + ~1-2 s session
/// initialization), so callers should hold a single instance for the
/// lifetime of the work. Cortex's daemon keeps one across embed ticks;
/// oracle keeps one process-local instance behind [`embed_query`].
///
/// `fastembed::TextEmbedding::embed` takes `&mut self`, so the inner
/// model lives behind a `Mutex` to keep the public surface `&self`. Embed
/// calls are CPU-bound but typically take ~10-50 ms; the lock window is
/// brief and bounded.
#[cfg(feature = "vec")]
pub struct FastEmbedModel {
    inner: Mutex<fastembed::TextEmbedding>,
    dim: usize,
    model_version: String,
}

#[cfg(feature = "vec")]
impl FastEmbedModel {
    /// Load the default `bge-small-en-v1.5` model.
    ///
    /// Cost: first call downloads ~100 MB to the fastembed cache (HF Hub)
    /// and initializes the ONNX runtime session (~1-2 s). Subsequent
    /// calls within the same process should reuse the returned handle.
    pub fn load() -> Result<Self> {
        let options = fastembed::TextInitOptions::new(fastembed::EmbeddingModel::BGESmallENV15);
        let model = fastembed::TextEmbedding::try_new(options)
            .map_err(|e| eyre::eyre!("failed to load bge-small-en-v1.5: {e}"))
            .wrap_err("FastEmbedModel::load")?;
        Ok(Self {
            inner: Mutex::new(model),
            dim: BGE_SMALL_EN_V15_DIM,
            model_version: BGE_SMALL_EN_V15_NAME.to_string(),
        })
    }
}

#[cfg(feature = "vec")]
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

#[cfg(feature = "vec")]
type RegistryEntry = (String, Arc<FastEmbedModel>);

/// Process-local registry of loaded models, keyed by `model_version`.
///
/// Oracle dispatches every query embedding through [`embed_query`]; the
/// first call for a given `model_version` performs the lazy load, stores
/// the `Arc<FastEmbedModel>` in the registry, and returns. Subsequent
/// calls hit the registry at ~0 ms overhead. Model bumps are rare, so
/// holding multiple models resident is acceptable.
#[cfg(feature = "vec")]
static MODEL_REGISTRY: OnceLock<RwLock<Vec<RegistryEntry>>> = OnceLock::new();

#[cfg(feature = "vec")]
fn registry() -> &'static RwLock<Vec<RegistryEntry>> {
    MODEL_REGISTRY.get_or_init(|| RwLock::new(Vec::new()))
}

/// Lazy-load a fastembed model for `model_version` and embed `text`.
///
/// Used by oracle's hybrid/vector dispatch where every query needs an
/// embedding and the model identifier comes from the DB's
/// `embedding_config.active_model`. Cortex does NOT use this; it loads a
/// concrete [`FastEmbedModel`] at the start of every `cortex embed`
/// invocation because every call uses inference, so the lazy registry
/// adds no value there.
///
/// Currently only `bge-small-en-v1.5` is recognized; passing any other
/// `model_version` returns an error. Adding new models requires explicit
/// support in this function because the dimension and model identifier
/// must round-trip through the rest of the pipeline.
#[cfg(feature = "vec")]
pub fn embed_query(text: &str, model_version: &str) -> Result<Vec<f32>> {
    let model = get_or_load_model(model_version)?;
    model.embed_one(text)
}

#[cfg(feature = "vec")]
fn get_or_load_model(model_version: &str) -> Result<Arc<FastEmbedModel>> {
    {
        let guard = registry()
            .read()
            .map_err(|_| eyre::eyre!("MODEL_REGISTRY read lock poisoned"))?;
        if let Some(entry) = guard.iter().find(|(k, _)| k == model_version) {
            return Ok(entry.1.clone());
        }
    }
    if model_version != BGE_SMALL_EN_V15_NAME {
        eyre::bail!(
            "unknown embedding model_version {model_version:?}; \
             only {BGE_SMALL_EN_V15_NAME:?} is currently supported"
        );
    }
    let model = Arc::new(FastEmbedModel::load()?);
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
/// Phase B1 of the hybrid retrieval design. Splits `text` into
/// overlapping chunks suitable for embedding. The window is measured
/// in whitespace-separated words, which is a coarse approximation of
/// the BPE token count. For English with the bge-small-en-v1.5
/// tokenizer the word:token ratio is ~0.75, so the default
/// `max_tokens = 400` yields chunks of about 530 BPE tokens - right
/// up against the model's 512-token limit. fastembed truncates
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
///   containing the full word; truncation falls to fastembed.
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

/// Deterministic test embedder.
///
/// Produces vectors derived from a 64-bit hash of the input text. The
/// hash is folded into the requested dimension and L2-normalized so the
/// output is compatible with cosine-similarity scoring (dot product on
/// unit vectors). Two different inputs produce different vectors; the
/// same input is stable across calls and across processes.
///
/// Used by Phase A3+ tests that need a real `EmbeddingModel` without the
/// ~1-2 s fastembed load cost.
pub struct MockEmbedder {
    dim: usize,
    model_version: String,
}

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
