# Design Document: Candle Embedding Backend

**Author:** Scott Idler
**Date:** 2026-05-17
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

Swap the embedding-model backend from fastembed (ONNX Runtime, AVX2-baseline) to candle-transformers (pure Rust, scalar fallback) so the hybrid retrieval pipeline runs on CPUs without AVX2. Keep both backends behind feature flags so a future move to a beefier machine restores fastembed's performance without code changes.

## Problem Statement

### Background

Doc 2 (`docs/design/2026-05-16-hybrid-retrieval-fts5-vector-rrf.md`) introduced FTS5 + vector + RRF hybrid retrieval. Phase A wired in `fastembed` (5.13.4) to produce `bge-small-en-v1.5` embeddings, gated behind the `vec` feature on the `vault` crate. Phases A through B4 shipped against `MockEmbedder` in tests and ran under the assumption that fastembed's bundled ONNX Runtime would Just Work on any x86_64 Linux box.

It does not. Pyke's pre-built ONNX Runtime binary (downloaded transparently by `ort-sys-2.0.0-rc.12/build/download/dist.txt`) is compiled with AVX2 as the CPU baseline. Loading the model on the development workstation (Intel `Genuine Intel(R) CPU @ 3.10GHz`, SIMD flags `mmx sse sse2 ssse3 sse4_1 sse4_2 avx popcnt`) terminates with exit code 132 (SIGILL — illegal instruction) the moment the runtime hits its first AVX2 op during BERT warmup. The model download succeeds (HF Hub returns 133 MB content); the inference engine never starts.

Confirmed empirically: after `otto deploy` of v0.6.1, `cortex embed --backfill` exits with signal 4 before writing a single row. `sqlite3 ~/.local/share/oracle/oracle.db "SELECT COUNT(*) FROM note_embeddings"` returns `0` against a vault of 1346 notes.

### Problem

Hybrid retrieval cannot ship on this CPU. The choice of fastembed bakes an AVX2 floor into the entire second-brain stack, with no Cargo feature flag to relax it.

### Goals

- `cortex embed --backfill` succeeds on the current workstation (no AVX2).
- `oracle` query-path embedding succeeds with the same backend.
- The `EmbeddingModel` trait surface stays unchanged; only impls and feature flags move.
- A future migration to an AVX2+ machine can restore fastembed (or use Candle's AVX2 or MKL paths) via feature flag, not code change.
- `model_version` encodes backend family so a backend swap does not silently mix incompatible vectors.

### Non-Goals

- GPU acceleration (CUDA/Metal). The vault is single-user and CPU is sufficient.
- Multi-model registry (more than one active model concurrently).
- Compiling ONNX Runtime from source. Considered and rejected (see Alternatives).
- Replacing the embedding model itself. We stay on `bge-small-en-v1.5`; only the inference engine changes.
- Re-litigating Doc 2's hybrid-retrieval design. This doc only swaps the embedding backend.
- Changing the embedding dimension (stays 384), the SQLite schema, the RRF code, or the `EmbeddingModel` trait surface.
- Adding a reranker stage. That belongs to a future doc.
- Hot-swapping backends at runtime. Backend choice is a compile-time feature flag.

## Proposed Solution

### Overview

Replace `FastEmbedModel` with a new `CandleBertModel` that implements the same `EmbeddingModel` trait. Both impls coexist behind Cargo feature flags. The default `vec` feature switches from fastembed to candle. The DB schema, the search/RRF code, and every test using `MockEmbedder` are untouched.

Backend identification is encoded in `model_version`:

| Backend family | model_version string             |
|----------------|----------------------------------|
| Candle         | `bge-small-en-v1.5-candle`       |
| fastembed/ONNX | `bge-small-en-v1.5-fastembed`    |

Switching kernels within a family (Candle scalar → AVX2 → MKL) does NOT change `model_version`; the fp32 drift across kernel variants is within cosine top-K tolerance. Switching families (Candle ↔ fastembed) DOES change `model_version` and triggers the existing stale-detection path in `vault::search::vector::stale_embedding_targets`, causing a full re-embed.

### Future Hardware Migration Path (Tier Ladder)

The whole point of doing the swap behind a feature flag, rather than ripping fastembed out and overwriting, is that this design must not paint us into a corner. If second-brain ever runs on different hardware — Scott's next workstation, a colleague's machine, a small VPS — we want the lowest-friction path to faster embeddings.

| Tier | Hardware             | Build                                                                       | Expected query embed latency |
|------|----------------------|-----------------------------------------------------------------------------|------------------------------|
| 0    | Pre-AVX2 (today)     | default (`vec` → `vec-candle`)                                              | ~150-300 ms (scalar Candle)  |
| 1    | AVX2 CPU             | `RUSTFLAGS="-C target-cpu=native"` + default features                       | ~25-50 ms (Candle AVX2)      |
| 2    | AVX2 + Intel MKL     | `--features vec-candle,candle-core/mkl` (requires MKL installed)            | ~10-20 ms (Candle + MKL BLAS)|
| 3    | AVX2 CPU             | `cargo install --no-default-features --features vec-fastembed`              | ~5-15 ms (ONNX MLAS)         |

No code change between tiers. The trait abstraction (`EmbeddingModel`), the `model_version` two-bucket scheme, and the feature-flag layout do all of this work. Moving from Tier 0 to Tier 1 is a `.cargo/config.toml` edit; from Tier 1 to Tier 3 is one `cargo install` flag.

### Architecture

```
vault/src/embedding.rs                   (existing entry; trait + registry)
vault/src/embedding/candle.rs            (new; CandleBertModel impl)
vault/src/embedding/fastembed.rs         (relocated from inline; gated)
```

Module split mirrors how the trait is consumed today: `embedding.rs` keeps the public `EmbeddingModel` trait, `MockEmbedder`, `chunk_transcript`, the `MODEL_REGISTRY`, and `embed_query`. Backend impls live in single-purpose submodules so feature gates are localised.

Feature wiring in `vault/Cargo.toml`:

```toml
[features]
schemars = ["dep:schemars"]
search = ["dep:rusqlite"]
watcher = ["dep:notify", "dep:tokio"]

# vec stays the public "embedding is on" flag. It pulls in `search` and
# the Candle backend (no AVX2 floor) so downstream crates that just say
# `--features vec` get a working install on any x86_64 CPU.
vec = ["vec-candle"]

# Backend-specific flags. Either pulls in `search` so a build that says
# `--features vec-fastembed` (no `vec`) still wires up the SQL layer.
# Enabling both is a compile error.
vec-candle = [
    "search",
    "dep:candle-core",
    "dep:candle-nn",
    "dep:candle-transformers",
    "dep:tokenizers",
    "dep:hf-hub",
]
vec-fastembed = [
    "search",
    "dep:fastembed",
]
```

Downstream crates (cortex / oracle / borg) keep `features = ["vec"]` and get Candle by default. An AVX2+ install uses `cargo install --no-default-features --features vec-fastembed`.

### Data Model

Unchanged. `note_embeddings` rows remain:

```sql
note_embeddings (
    id INTEGER PRIMARY KEY,
    note_path TEXT NOT NULL,
    kind TEXT NOT NULL,          -- 'summary' | 'transcript-chunk'
    chunk_index INTEGER NOT NULL,
    text TEXT NOT NULL,
    embedding BLOB NOT NULL,     -- f32[384] little-endian
    model_version TEXT NOT NULL, -- new value: bge-small-en-v1.5-candle
    source_modified_at INTEGER NOT NULL
)
```

`embedding_config.active_model` (the per-DB pin) gets seeded to `bge-small-en-v1.5-candle` on a fresh DB. The existing seed value in `vault::search::SearchIndex::open_*` changes accordingly.

### API Design

The `EmbeddingModel` trait stays as-is:

```rust
pub trait EmbeddingModel: Send + Sync {
    fn dim(&self) -> usize;
    fn model_version(&self) -> &str;
    fn embed_one(&self, text: &str) -> Result<Vec<f32>>;
    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
}
```

The Candle impl:

```rust
#[cfg(feature = "vec-candle")]
pub struct CandleBertModel {
    model: candle_transformers::models::bert::BertModel,
    tokenizer: tokenizers::Tokenizer,
    device: candle_core::Device,
    dim: usize,
    model_version: String,
}

impl CandleBertModel {
    /// Download (if needed) and load BAAI/bge-small-en-v1.5 from the
    /// HuggingFace Hub. Cache lives under hf-hub's default location
    /// (~/.cache/huggingface/hub/) so it survives binary upgrades.
    pub fn load() -> Result<Self>;
}
```

`embed_one` / `embed_batch`:

1. Tokenize via `tokenizers::Tokenizer::encode_batch`, truncate to 512 tokens (`bge-small-en-v1.5` hard limit per the model card), pad to longest-in-batch.
2. Build `input_ids`, `token_type_ids` (all zeros for single-sentence input), and `attention_mask` tensors.
3. Forward through `BertModel`. The output is `[batch, seq, 384]`.
4. **CLS pool:** take token 0 only — `output.i((.., 0, ..))`. This is the canonical BGE pooling strategy per the model card (`model_output[0][:, 0]`). Mean pooling, which the upstream candle BERT example uses, is wrong for BGE.
5. **L2 normalize** along the embedding axis.
6. Return `Vec<Vec<f32>>` with shape `[batch][384]`.

Query-side: BGE v1.5's model card states the optional instruction prefix ("Represent this sentence for searching relevant passages:") "only has a slight degradation in retrieval performance" when omitted, and "you can generate embedding without instruction in all cases for convenience." We omit it, matching fastembed's behavior and keeping the embed path symmetric between corpus and query.

Prefetch:

```rust
/// `cortex embed --prefetch-model` calls this. Downloads tokenizer.json
/// config.json and model.safetensors via hf-hub. Idempotent — re-runs are
/// a no-op once cached.
pub fn prefetch_bge_small() -> Result<()>;
```

### Implementation Plan

**Picking this up cold?** Read Doc 2 (`docs/design/2026-05-16-hybrid-retrieval-fts5-vector-rrf.md`) for the hybrid-retrieval context this design swaps the embedding backend within. Then run `cortex embed --backfill` on a pre-AVX2 CPU to reproduce the SIGILL crash if you want to see the root problem firsthand. Then start at Phase 1. Commit at the end of each phase (`bump` runs at the end of Phase 7 only).

**Workspace-root reminder.** `bump` and `otto` both need to run from the workspace root (`~/repos/scottidler/second-brain`), not from a subcrate. `cargo` commands work from anywhere; `cargo add --package vault <crate>` lets you add deps to vault from the root.

#### Phase 1: Dependency swap and module scaffolding
**Model:** sonnet
**Files touched:** `vault/Cargo.toml`, `vault/src/embedding.rs`, `vault/src/embedding/candle.rs` (new), `vault/src/embedding/fastembed.rs` (extracted)

- `cargo add --package vault candle-core candle-nn candle-transformers tokenizers hf-hub` (use whatever latest each picks up; do not pin to training-data versions).
- Edit `vault/Cargo.toml` features section per the diff above.
- Split `vault/src/embedding.rs`:
  - Move `FastEmbedModel` and its impl into `vault/src/embedding/fastembed.rs` behind `#[cfg(feature = "vec-fastembed")]`.
  - Create empty `vault/src/embedding/candle.rs` behind `#[cfg(feature = "vec-candle")]` with the struct skeleton above.
- Add the exactly-one-backend compile guard in `embedding.rs`:

  ```rust
  #[cfg(all(feature = "vec-candle", feature = "vec-fastembed"))]
  compile_error!("Enable exactly one embedding backend, not both");
  ```

  The mirror guard ("vec without a backend") is unnecessary: `vec`'s feature list mandates `vec-candle`, so the unsatisfied state is unreachable.

- Update `vault::search::SearchIndex::open_*` to seed `embedding_config.active_model` from a `const` driven by the active backend feature.
- Verify all four states compile cleanly:
  ```bash
  cargo check -p vault --features vec                                        # default: vec → vec-candle
  cargo check -p vault --no-default-features --features search,vec-fastembed # legacy
  cargo check -p vault --no-default-features --features search,vec-candle    # explicit-candle
  cargo check -p vault                                                       # no embedding at all
  ```
  The "both backends at once" case should produce a `compile_error!` from the guard added in this phase — verify the error message is clear.

#### Phase 2: CandleBertModel core
**Model:** opus
**Files touched:** `vault/src/embedding/candle.rs`

Implementation skeleton (annotated for the implementer; final code will deviate in details but not in shape):

```rust
use candle_core::{Device, Tensor, DType};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config, DTYPE};
use eyre::{Result, WrapErr};
use hf_hub::api::sync::Api;
use std::sync::Mutex;
use tokenizers::{Tokenizer, PaddingParams, TruncationParams};

const MODEL_REPO: &str = "BAAI/bge-small-en-v1.5";
const MODEL_VERSION_STR: &str = "bge-small-en-v1.5-candle";
const DIM: usize = 384;
const MAX_SEQ_LEN: usize = 512;

pub struct CandleBertModel {
    inner: Mutex<Inner>,
    model_version: String,
}

struct Inner {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
}

impl CandleBertModel {
    pub fn load() -> Result<Self> {
        let api = Api::new().wrap_err("hf-hub Api::new")?;
        let repo = api.model(MODEL_REPO.into());
        let config_path = repo.get("config.json")?;
        let tokenizer_path = repo.get("tokenizer.json")?;
        let weights_path = repo.get("model.safetensors")?;

        let config: Config = serde_json::from_str(&std::fs::read_to_string(config_path)?)?;
        assert_eq!(config.hidden_size, DIM, "BGE config hidden_size drifted");

        let device = Device::Cpu;
        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(&[weights_path], DTYPE, &device)?
        };
        let model = BertModel::load(vb, &config)?;

        let mut tokenizer = Tokenizer::from_file(tokenizer_path)
            .map_err(|e| eyre::eyre!("tokenizer load: {e}"))?;
        tokenizer.with_truncation(Some(TruncationParams {
            max_length: MAX_SEQ_LEN,
            ..Default::default()
        })).map_err(|e| eyre::eyre!("truncation params: {e}"))?;
        // Padding parameters get set per-batch when batch.len() > 1; single-input
        // path skips padding entirely.

        Ok(Self {
            inner: Mutex::new(Inner { model, tokenizer, device }),
            model_version: MODEL_VERSION_STR.to_string(),
        })
    }

    fn embed_inner(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut g = self.inner.lock().map_err(|_| eyre::eyre!("mutex poisoned"))?;
        // tokenize → stack input_ids / token_type_ids / attention_mask
        // → model.forward → CLS pool (output[:, 0, :]) → L2 normalize
        // → flatten to Vec<Vec<f32>>
        todo!()
    }
}

impl EmbeddingModel for CandleBertModel {
    fn dim(&self) -> usize { DIM }
    fn model_version(&self) -> &str { &self.model_version }
    fn embed_one(&self, text: &str) -> Result<Vec<f32>> {
        let mut v = self.embed_inner(&[text])?;
        v.pop().ok_or_else(|| eyre::eyre!("empty embed result"))
    }
    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        self.embed_inner(texts)
    }
}
```

Implementation notes:
- The pooling step is `output.i((.., 0, ..))` (CLS token at position 0), not mean pooling. The upstream candle BERT example demonstrates mean pooling for `BAAI/bge-large-zh-v1.5` — that example is wrong for the standard BGE recipe; the BGE model card specifies `model_output[0][:, 0]`.
- L2 normalize after pooling: `embeddings.broadcast_div(&embeddings.sqr()?.sum_keepdim(D::Minus1)?.sqrt()?)`.
- Serde ignores unknown fields by default, so BGE's config.json extras (`_name_or_path`, `architectures`, `attention_probs_dropout_prob`, `id2label`, `label2id`, `torch_dtype`, `transformers_version`) deserialize cleanly into Candle's `Config`. If a future Candle release adds `#[serde(deny_unknown_fields)]`, hand-roll a Config deserializer with the fields enumerated in the model card (~20 LOC).
- **Attention mask is mandatory, not optional.** With variable-length inputs in a batch, padded positions must be excluded from the attention computation. Construct it as `attention_mask = input_ids.ne(pad_token_id)?.to_dtype(DType::U32)?` (shape `[batch, seq]`, 1 for real tokens, 0 for padding). Pass it to `BertModel::forward(input_ids, token_type_ids, Some(&attention_mask))`. Skipping it would silently corrupt the CLS-pooled embedding for padded sequences — exactly the kind of bug the Phase 3 numerical-parity test exists to catch, but the better fix is to not introduce it. `pad_token_id` comes from `config.pad_token_id` (BGE's is 0).
- `token_type_ids` is all-zeros for single-sentence input: `token_ids.zeros_like()?`. BGE doesn't use sentence-pair training.
- The inner `Mutex` matches the existing `FastEmbedModel` pattern: forward needs `&self`, but Candle's tensor pipeline keeps internal buffers that complicate `Sync` without an explicit lock. The lock window is brief (~150-300 ms per embed call) and bounded. Note that Phase 6 holds *multiple* `Mutex<Inner>` instances (one per internal worker), each independently lockable.
- **`BertModel` cloneability is unverified at this candle version (0.10.2).** If `BertModel: Clone` is implemented and shares the underlying `Arc<TensorImpl>` weight buffers, Phase 6's pool can construct replicas via `Arc::new(model.clone())`. If `BertModel: !Clone`, fall back to calling `Inner::load()` N times — each load mmaps the same `model.safetensors` file via `VarBuilder::from_mmaped_safetensors`, and the OS page cache deduplicates the physical pages across the N memory maps. Either way, total RSS for the weights stays at ~133 MB regardless of replica count. The implementer should verify this empirically with `ps aux` after constructing the pool; if RSS climbs by N×133 MB, the clone path is duplicating weight buffers and the mmap fallback is required.

#### Phase 3: Numerical parity verification
**Model:** opus
**Files touched:** `vault/tests/regression/candle_parity.rs` (new), `vault/tests/fixtures/bge-reference.json` (new), `vault/tests/regression.rs` (add module declaration)

The model card doesn't publish reference vectors, so generate them yourself on any machine with `sentence-transformers`:

```python
# bin/gen-bge-reference.py  (commit alongside the fixture for reproducibility)
from sentence_transformers import SentenceTransformer
import json

model = SentenceTransformer("BAAI/bge-small-en-v1.5")
texts = [
    "The capital of France is Paris.",
    "Mango is a tropical fruit.",
    "Hybrid retrieval combines BM25 with vector search via reciprocal rank fusion.",
]
embeddings = model.encode(texts, normalize_embeddings=True).tolist()
json.dump({"texts": texts, "embeddings": embeddings}, open("vault/tests/fixtures/bge-reference.json", "w"))
```

Run that once on a workstation that already has `sentence-transformers` installed (or `pipx install sentence-transformers && python bin/gen-bge-reference.py`); commit the resulting JSON. The fixture is small (~5 KB) and stable as long as we stay on bge-small-en-v1.5.

Test body:

```rust
// vault/tests/regression/candle_parity.rs
#[test]
fn candle_bert_matches_sentence_transformers_reference() {
    let fixture: Fixture = serde_json::from_str(
        include_str!("../fixtures/bge-reference.json")
    ).expect("fixture parse");
    let model = CandleBertModel::load().expect("load");
    for (text, expected) in fixture.texts.iter().zip(fixture.embeddings.iter()) {
        let got = model.embed_one(text).expect("embed");
        let cos_dist = 1.0 - dot(&got, expected); // both L2-normalized
        assert!(
            cos_dist < 1e-3,
            "candle drift on '{text}': cos_dist={cos_dist:.6} (limit 1e-3)"
        );
    }
}
```

The `< 1e-3` cosine-distance tolerance gives room for fp32 kernel drift between PyTorch CPU and Candle scalar, but is tight enough to catch:

1. Mean pooling instead of CLS (cos_dist would be ~0.3-0.6)
2. Missing L2 normalize (norms differ → cos_dist arbitrary)
3. Attention mask not propagated (~0.05-0.2 drift on padded inputs)
4. Wrong `token_type_ids` shape (~0.01-0.05 drift)
5. hf-hub returned a different revision (catastrophic; cos_dist near 1.0)

If the test fails, walk that list in order. Don't ship until it's green.

#### Phase 4: Registry, embed_query, prefetch rewiring
**Model:** sonnet
**Files touched:** `vault/src/embedding.rs` (registry types), `cortex/src/embed.rs` (prefetch flow + load call site), `oracle/` (only if it constructs an EmbeddingModel directly — verify; otherwise no change)

- Change `MODEL_REGISTRY` and `get_or_load_model` to hold `Arc<CandleBertModel>` (gated by `vec-candle`) and `Arc<FastEmbedModel>` (gated by `vec-fastembed`). The trait object alternative is rejected because the existing code uses concrete types.
- Cleanest approach: introduce a type alias `pub type ActiveModel = CandleBertModel;` (or `FastEmbedModel`) gated by feature; the registry holds `Arc<ActiveModel>`. Cortex and oracle don't see the change.
- `embed_query` body is unchanged.
- Replace cortex's `--prefetch-model` flow:
  - Today it calls `FastEmbedModel::load()` to force a download.
  - New path calls `CandleBertModel::prefetch_bge_small()` which uses `hf_hub::api::sync::Api` directly for the three files.

#### Phase 5: model_version migration and seed
**Model:** sonnet
**Files touched:** `vault/src/search.rs` (or wherever the schema seed lives), `vault/src/search/vector/tests.rs`, any other test that pins the old `bge-small-en-v1.5` string

- The DB currently has 0 embeddings (confirmed empirically). No data migration is needed.
- The seed value in `vault::search::SearchIndex` schema setup changes from `"bge-small-en-v1.5"` to `"bge-small-en-v1.5-candle"`.
- The legacy seed value lives in two test files (verify via `grep`); update them to the new value where tests are backend-agnostic, or to a backend-specific value where they exercise a specific backend. Specifically, `active_embedding_model_reads_the_default_seed` (in `vault/src/search/vector/tests.rs`) currently asserts `"bge-small-en-v1.5"` — update to `"bge-small-en-v1.5-candle"`.
- The schemars / MCP-facing surface needs no change (model_version is opaque to clients).
- Add a defensive check in `get_or_load_model`: if the caller passes a `model_version` that does NOT match the compiled backend's `MODEL_VERSION_STR`, return an eyre error like `unknown or backend-mismatched model_version: '{x}' (this binary expects: bge-small-en-v1.5-candle)`. This prevents silent misuse when a downstream caller has stale hardcoded strings.

#### Phase 6: Internal parallelism inside `CandleBertModel::embed_batch`
**Model:** opus
**Files touched:** `vault/src/embedding/candle.rs`

`cortex/src/embed.rs` documents a load-bearing transaction-discipline contract (Phase A5): each batch must be a discrete READ (auto-commit) → INFERENCE (no SQLite lock) → WRITE (one short `BEGIN IMMEDIATE` transaction, under ~200 ms wall-clock at `batch_size = 64`). The Phase A5 regression test asserts this. Parallelisation must NOT touch that loop's shape — adding a 1346-row transaction or holding the write lock across inference would starve oracle's `index_vault` writes.

**Therefore the pool lives inside `CandleBertModel`, not inside `cortex::embed`.** `CandleBertModel::embed_batch(&[...])` becomes internally parallel; `cortex::embed`'s 3-phase loop is unchanged. The trait abstraction is preserved (Tier 3 fastembed still owns its own threading via ONNX MLAS), the transaction discipline is preserved (each batch of 64 still flushes in one short tx), and the parallelism gain is recovered (64 inputs in one `embed_batch` call now fan out across 8 internal workers).

Design inside `CandleBertModel`:

```
                ┌──────────────────────────────────────────────┐
                │  CandleBertModel.embed_batch(&[t1..t64])     │
                │  1. tokenize all inputs                      │
                │  2. split into K sub-chunks                  │
                │  3. rayon::par_iter over sub-chunks ─┐       │
                │  4. flatten results in input order            │
                └──────────────────────────────────────────────┘
                                                       │
                                                       ▼
                ┌──────────────────────────────────────────────┐
                │  Per worker (rayon):                         │
                │  • acquires its own Mutex<Inner> via         │
                │    round-robin index (AtomicUsize)           │
                │  • calls Inner.forward(&sub_chunk_tensors)   │
                │  • returns Vec<[384]f32>                     │
                └──────────────────────────────────────────────┘
```

Implementation steps:

1. **Internal replica vector.** `CandleBertModel` holds `Vec<Mutex<Inner>>` (length N), not a single `Mutex<Inner>`. Each `Inner` has its own `BertModel` clone and its own `Tokenizer`. Candle's `Tensor` is internally `Arc<TensorImpl>`, so `BertModel` cloning should share weight buffers; verify by sampling RSS during load. If `BertModel: !Clone`, fall back to `Inner::load()` invoked N times — `VarBuilder::from_mmaped_safetensors` uses the same on-disk safetensors file each time, and the OS page cache deduplicates the physical pages so RAM stays at ~133 MB regardless of N.
2. **Round-robin replica selection.** `next_replica_index: AtomicUsize` — each `embed_batch` call atomically fetches and increments, modulo `replicas.len()`. Splitting a 64-item batch across 8 replicas means each replica handles 8 inputs in its own forward pass, all 8 happening concurrently because each holds its own mutex.
3. **Sub-chunking.** Inside `embed_batch`, compute `sub_chunk_size = batch.len().div_ceil(replicas.len())`. Use `batch.par_chunks(sub_chunk_size).enumerate().map(|(i, sub)| ...)` to dispatch. Results collect in order via the enumerate index.
4. **Tiny-workload guard.** If `batch.len() <= 1`, run synchronously on `replicas[0]` to avoid the rayon dispatch overhead. For the daemon's steady-state path (~one note at a time during ingest), this gives single-replica behavior automatically.
5. **Tokio integration.** Callers from inside a tokio task wrap with `tokio::task::block_in_place(|| model.embed_batch(...))`. This is the project's established pattern for rayon work invoked from async contexts. `cortex::embed::run_embed` already runs in a sync function from the daemon, so the wrapping happens at the daemon-tick layer (verify and add if missing).
6. **Worker count knob.** Add `embed.workers: usize` to `cortex.yml` (default `min(8, num_physical_cores)`), and on `CandleBertModel::load()` accept the count. Cap at 8 to keep peak RSS bounded.

Cortex/oracle remain backend-agnostic: they just call `model.embed_batch(&texts)` and the trait impl decides internally whether to parallelise. The `FastEmbedModel` impl keeps its single-mutex shape — fastembed's ONNX MLAS already spawns its own threads, so a second layer of replication there would oversubscribe the CPU.

Expected wall-clock for 1346 notes on this CPU (single `cortex embed --backfill` invocation, batch_size = 64, 21 batches):

| internal workers | scalar Candle | notes                                  |
|------------------|---------------|----------------------------------------|
| 1                | ~270 s        | baseline; current single-threaded path |
| 4                | ~70 s         | ~95% scaling efficiency                |
| 8                | ~37 s         | ~90% scaling efficiency                |
| 16               | ~25 s         | hyperthread saturation; ~70% scaling   |

Default 8 is the sweet spot. RAM peak well under 200 MB additional (the 133 MB of weights is shared across replicas via Arc'd tensors or mmap-dedup'd pages; the per-replica `Mutex<Inner>` overhead is in the kilobytes).

Verification:
- Existing `cortex embed --backfill` is the end-to-end test; rerun it after Phase 6 and confirm wall-clock drops from ~5 min to ~35-45 sec.
- Add a unit test inside `vault/src/embedding/candle/tests.rs` (matching the project's test layout) that asserts `embed_batch(&[a,b,c,d,e,f,g,h])` with 4 internal workers produces the same vectors (within fp32 tolerance) as 8 calls to `embed_one`.
- The existing Phase A5 transaction-discipline regression test in `cortex/` continues to pass unchanged because `cortex::embed`'s loop shape did not change.

Daemon path: the cortex embed tick runs every 10 min by default. Steady-state work is small (~20 ingests/day, batches of 1-2). The tiny-workload guard means single-replica path; no daemon-specific worker tuning needed.

#### Phase 7: Deploy and backfill
**Model:** sonnet
**Files touched:** none in-tree; runtime steps only

- Bump version via `bump -m` (minor: 0.6.1 → 0.7.0). The model_version change is a non-backwards-compat semantic event even though no embeddings exist yet — minor bump signals the change. Note Scott's bump skill: `bump -m` for minor, `bump` for patch.
- `git push && git push --tags`.
- `otto deploy` (installs all three crates, restarts borg + cortex).
- `cortex embed --prefetch-model` to warm the HF Hub cache.
- `cortex embed --backfill` against the live vault (~1346 notes). Expected wall-clock with Phase 6's internal 8-replica pool inside `CandleBertModel::embed_batch`: ~35-45 sec for the summary pass; transcript-chunk pass is variable (depends on how many transcript-eligible kinds carry a populated `## Transcript` section after Phase B2's backfill). Without Phase 6 (single-replica fallback), summary pass is ~4-5 min.
- Verify rowcount per kind separately, since transcript-chunk rows add to the total:

  ```sql
  SELECT kind, COUNT(*) FROM note_embeddings GROUP BY kind;
  ```

  Expectations: `summary` row count ≈ 1346 (one per note), `transcript-chunk` row count = sum of chunks across transcript-eligible notes (variable; for a small vault probably tens to low hundreds).
- Smoke-test oracle: `oracle` MCP call with `knowledge_search { query: "...", mode: "hybrid" }` returns results. Note the latency budget: scalar Candle query-side embedding adds ~150-300 ms on top of the ~50-150 ms BM25+RRF path, putting p50 hybrid latency in the ~200-450 ms range on this CPU. The criterion bench in `vault/benches/hybrid.rs` reports 13.7 ms p50 today, but that uses `MockEmbedder` and does not include real model inference — it measures only BM25 + cosine + RRF.

## Alternatives Considered

### Alternative 1: Build ONNX Runtime from source, keep fastembed

- **Description:** Clone `microsoft/onnxruntime`, build with `--cmake_extra_defines onnxruntime_USE_AVX2=OFF onnxruntime_USE_AVX512=OFF` for a non-AVX2 baseline. Switch fastembed feature from `ort-download-binaries-native-tls` to `ort-load-dynamic`, set `ORT_DYLIB_PATH` to the local build.
- **Pros:** Keeps existing fastembed code path with no Rust changes. Once compiled, ONNX MLAS kernels remain the fastest CPU BERT engine.
- **Cons:** ~30-min source build, fragile on every ONNX Runtime upgrade, only solves the symptom on this one machine. A future contributor or new machine inherits the same problem, and the build script isn't a project-owned artifact.
- **Why not chosen:** Solves the symptom without removing the dependency on a fragile environmental assumption. Doesn't generalise to future hardware diversity.

### Alternative 2: External embedding API (Cohere, OpenAI ada, Voyage)

- **Description:** Replace local embedding with an HTTP call to an external service.
- **Pros:** No local compute at all. Always-current model.
- **Cons:** Breaks second-brain's local-first invariant. Adds latency, cost, network failure modes. Requires API key management. Vendor lock-in on a vector space that may shift between API versions.
- **Why not chosen:** The whole point of this stack is local-first knowledge retrieval. External embedding adds an outage axis that swallows the design philosophy.

### Alternative 3: Stay on fastembed but switch to a smaller / non-BERT model

- **Description:** Use a model whose ONNX export doesn't need AVX2 (e.g. quantised int8 model that uses simpler kernels).
- **Pros:** Keeps fastembed.
- **Cons:** Speculative — there's no documented fastembed model whose ONNX export skips AVX2. The model would be lower-quality on retrieval benchmarks. We'd lose `bge-small-en-v1.5`'s strong MTEB scores.
- **Why not chosen:** No evidence such a fastembed-shipped model exists, and degrading retrieval quality for a hardware workaround is the wrong trade.

### Alternative 4: Hybrid — Candle for embedding, fastembed for reranking

- **Description:** Use Candle for the bulk embed pipeline, keep fastembed for any future reranker.
- **Pros:** None concrete today; we don't have a reranker.
- **Cons:** Premature complexity.
- **Why not chosen:** Reranking is not in scope for second-brain right now. If it ever is, that's its own design doc.

## Technical Considerations

### Dependencies

New (gated behind `vec-candle`):

| Crate                | Purpose                              |
|----------------------|--------------------------------------|
| candle-core          | Tensor ops, Device abstraction       |
| candle-nn            | Layers (used transitively by bert)   |
| candle-transformers  | `models::bert::{BertModel, Config}`  |
| tokenizers           | BPE / WordPiece tokenization         |
| hf-hub               | Model file download from HF Hub      |

Removed-by-default (still available behind `vec-fastembed`):

| Crate                | Purpose                              |
|----------------------|--------------------------------------|
| fastembed            | ONNX-backed embedding (legacy path)  |

Transitive impact: `candle-core` is already in our dep tree (fastembed transitively pulls it for nomic/qwen3 paths). `hf-hub` likewise. So the actual new transitive crates are a small set.

### Performance

**Known budget regression.** Doc 2 set a p50 hybrid-retrieval budget of 200 ms wall-clock. With Candle scalar embedding on this CPU, real-world query p50 lands in the 200-450 ms range — at or slightly over budget. (The query path embeds a *single* string per call, which means Phase 6's internal pool degrades to single-replica via the tiny-workload guard; the parallelism only helps backfill, not per-query latency.) This is an explicit, documented regression scoped to the pre-AVX2 deployment and accepted in exchange for "embedding works at all on this hardware". A future Tier 1+ migration (see Future Hardware Migration Path above) recovers the budget. The criterion bench `vault/benches/hybrid.rs` currently reports 13.7 ms p50 but uses `MockEmbedder` and therefore does *not* exercise the real budget; the implementer should add a Candle-backed bench variant alongside Phase 2 so the real number is captured under criterion's statistical sampler.

Scalar BertModel inference on bge-small-en-v1.5 (12 layers × 384 hidden × 1536 intermediate × 12 heads) on a 3.1 GHz pre-AVX2 CPU, batch size 1, ~100-token input: estimated 100-300 ms per inference. Sources: candle's own benchmarks show ~3-8× slowdown for scalar vs AVX2 BERT, and AVX2-fastembed lands around 30-60 ms for this model on this token count.

Backfill cost: 1346 notes × ~200 ms = ~4-5 min wall-clock single-threaded. Phase 6's internal parallelism inside `CandleBertModel::embed_batch` (8 replicas, each holding their own `Mutex<Inner>`) cuts that to ~35-45 sec on this 32-thread workstation, well-amortised against the one-shot nature. The `cortex::embed` loop shape stays unchanged so the Phase A5 transaction-discipline contract (200 ms write tx, 64 rows per batch) holds. Steady-state daemon embedding (~20 ingests/day per `project_state` memory) is invisible regardless of internal-worker count (~4 sec/day; the tiny-workload guard runs single-replica when batch ≤ 1).

Query-path embedding adds 100-300 ms to oracle's `knowledge_search` calls. Today the BM25 portion is ~50-150 ms; the vector portion ≪10 ms once BM25 finishes; the criterion bench shipped in v0.6.1 (`vault/benches/hybrid.rs`) reports 13.7 ms p50 — but that bench uses `MockEmbedder` and excludes real model inference, so it does not validate the 200 ms p50 design budget under realistic conditions. The unmeasured embedding cost is the difference between fastembed (~30-60 ms on AVX2) and Candle scalar (~150-300 ms on this CPU). Real-world p50 with Candle scalar lands in the ~200-450 ms range, **at the boundary of or slightly over Doc 2's original 200 ms p50 budget on this hardware**. Acceptable trade given the alternative is "doesn't work at all"; must be documented as a known regression scoped to the pre-AVX2 deployment. A future Tier 1 recompile with `target-cpu=native` on a newer machine recovers most of this. A follow-up bench should extend `hybrid.rs` with a Candle-backed variant once Phase 2 lands so the real budget number is captured under criterion's statistical sampler.

Memory: BertModel weights for bge-small are ~133 MB on disk; in-process is comparable (we mmap via VarBuilder). Existing cortex daemon already has fastembed loaded; switching costs roughly equal RAM.

### Security

The model is fetched from `huggingface.co` (and pyke's CDN today, soon HF only). This is the same trust boundary as fastembed; no change. The model file is a `safetensors` blob, not pickled Python, so deserialization is not a code-execution vector. The `tokenizer.json` is a JSON-defined tokenizer specification (no executable code).

`hf-hub` does HTTPS with rustls or native-tls; we pick whichever default. SHA verification of downloaded blobs is on by default (hf-hub validates against the HF Hub-supplied SHA256).

### Testing Strategy

- **Unit tests:** Use `MockEmbedder` everywhere they do today. Zero change.
- **Numerical parity test (Phase 3):** New integration test asserting cosine distance < 1e-3 against reference vectors for a handful of canonical inputs.
- **Backend-skew test:** A test that exercises both impls behind their respective feature flags would be ideal but is awkward in cargo's feature-resolution model. Skip for now; rely on the feature gates and the compile_error guards.
- **CI:** otto ci already runs `cargo test --features vec` which now exercises Candle. Add an `otto ci-fastembed` task (or a manual workflow) that runs `cargo test --no-default-features --features search,vec-fastembed` on machines that have AVX2 — important once any contributor builds the fastembed path.
- **End-to-end:** `cortex embed --backfill` + an `oracle knowledge_search` call after deploy is the smoke test. Codify as a one-line shell script under `bin/` if it becomes repetitive.

### Rollout Plan

1. Branch and implement Phases 1-5.
2. Local verify: `otto ci` green; numerical parity test passes; manual `cortex embed --backfill` against a copy of the vault.
3. Bump to v0.7.0 via `bump -m` (minor — model_version semantics change).
4. `git push && git push --tags`.
5. `otto deploy` to install + restart daemons.
6. `cortex embed --prefetch-model`.
7. `cortex embed --backfill` against live vault. Verify rowcount.
8. Manual smoke test via oracle.
9. Update `MEMORY.md` `project_state` entry with the new version and the backend-swap fact.

## Risks and Mitigations

| Risk                                                                      | Likelihood | Impact | Mitigation                                                                                                                                |
|---------------------------------------------------------------------------|------------|--------|-------------------------------------------------------------------------------------------------------------------------------------------|
| Candle's `bert::Config` rejects BGE's `attention_probs_dropout_prob` or other extra fields | Med        | Low    | Serde ignores unknown fields by default. If a future Candle release adds `deny_unknown_fields`, hand-roll a Config deserializer (~20 LOC).|
| Numerical parity test fails — wrong pooling or normalisation              | Med        | High   | Phase 3 explicitly tests CLS pooling against reference vectors. Diagnose via the ordered checklist in Phase 3 before proceeding.          |
| Scalar inference slower than 300 ms/note                                  | Low        | Med    | Backfill is one-shot; we can tolerate 10-15 min wall-clock. If query-path latency becomes a real complaint, prioritise the AVX2 migration.|
| Future fastembed swap-back regresses on dimension or model_version logic  | Low        | Low    | Both backends keep `dim() == 384`. `model_version` strings are explicit constants; CI on AVX2 hosts will catch any wiring breakage.       |
| hf-hub API churn (it's at `1.0.0-rc.1` today)                             | Med        | Low    | Pin via Cargo.lock. Upgrade deliberately. The hf-hub surface we touch is tiny (`Api::new`, `Repo::with_revision`, `api.get(...)`).        |
| Tokeniser truncation drops important content                              | Low        | Low    | 512-token cap is identical to fastembed's. The transcript-chunk path in Doc 2 already pre-chunks long transcripts; per-row text is short. |
| Candle scalar path itself uses target_feature gates we don't satisfy      | Low        | High   | Verified in candle source: `#[cfg(not(any(target_feature = "neon", target_feature = "avx2", target_feature = "simd128")))]` covers us.    |
| Existing `cortex embed --prefetch-model` users' invocation breaks         | Low        | Low    | Same CLI flag, same exit semantics. Internal implementation changes only.                                                                 |
| BertModel struct clone duplicates weight buffers instead of Arc-sharing them, blowing memory to 8 × 133 MB ≈ 1 GB | Low | Med | Verified post-pool construction via RSS sampling in Phase 6. If duplication is happening, fall back to `min(4, num_physical_cores)` replicas and document the RAM cost. Tensors in Candle are internally Arc'd, so this should not trigger. |
| Worker uses a shared Mutex by accident, silently serialising the pool     | Med        | High   | Each `Arc<CandleBertModel>` holds its own `Mutex<Inner>` per Phase 2's design. Phase 6's chunk-dispatch code uses round-robin replica selection — explicitly NOT a single shared lock. Phase 6 includes an assertion test that 8-worker wall-clock is < 2x single-worker wall-clock; if it isn't, the lock structure is wrong. |
| `MODEL_REGISTRY` lookup with stale `model_version` string (e.g. legacy `bge-small-en-v1.5` without the `-candle` suffix) | Med | Med | `get_or_load_model` returns a clear error: `unknown model_version: '{x}' (expected one of: bge-small-en-v1.5-candle)`. Oracle surfaces this to the MCP client. Callers should read `model_version` from the DB's `embedding_config.active_model` row rather than hardcode it. |
| User builds `--features vec-fastembed` on a pre-AVX2 machine and re-encounters SIGILL | Low | High | `FastEmbedModel::load` adds an opportunistic CPU-feature probe at the top: if `cfg!(target_arch = "x86_64")` and the runtime CPU lacks AVX2 (via `std::arch::is_x86_feature_detected!("avx2")`), return an eyre error pointing the user at the `vec-candle` feature. Cheap insurance against the exact regression this design fixes. |
| Generated `bge-reference.json` fixture goes stale (new BGE checkpoint, new texts) | Low | Low | The fixture's accompanying script `bin/gen-bge-reference.py` documents the regeneration recipe. The fixture's correctness binds to the specific texts in the file; changing them requires re-running the script. CI does not re-generate. |
| Candle scalar inference saturates rayon's global thread pool, stalling concurrent sweeps (autotag, quality, migrate, audit) | Med | Low | The cortex daemon serialises these sweeps via its `tokio::select!` loop — they don't run concurrently with `embed`. The practical impact is "a sweep triggered while a backfill is mid-flight waits for it to finish" (~35-45 sec extra), not a deadlock or crash. Acceptable. If a future daemon shape runs sweeps concurrently with embed, revisit by giving the embed pool its own dedicated rayon `ThreadPoolBuilder` instead of the global. |
| Phase 6's internal pool is leaked into `cortex::embed` instead of staying inside `CandleBertModel`, breaking the trait abstraction for the Tier 3 fastembed path | Low | High | Phase 6's design explicitly keeps the pool *inside* `CandleBertModel::embed_batch`. `cortex::embed`'s 3-phase loop and the Phase A5 transaction-discipline test are untouched. `FastEmbedModel::embed_batch` keeps its single-mutex shape and relies on ONNX MLAS's internal threading; the trait surface is the only contract `cortex::embed` knows about. |

## Open Questions

- [ ] Should we delete the fastembed code path entirely once Candle ships, or keep it gated as designed? **Default answer:** keep it gated. The tier ladder (scalar → AVX2 → MKL → fastembed) is real, the cost of a gated impl is one feature flag and ~150 LOC, and ripping it out commits us to never using ONNX again — too narrow a bet for a project that may run on different hardware in the future.
- [ ] Worth adding an `[[bin]]` target like `bin/candle-smoke` that loads BGE and embeds one string, as a portable "does this CPU work?" probe future contributors can run before deploying? **Default answer:** defer. Low cost but no current consumer; revisit once a second machine cares.
- [ ] Should `bump -m` be `bump -M` (major) instead, since the model_version scheme is non-backwards-compatible? **Default answer:** minor. The DB has zero embeddings today; no rows are invalidated. Major is reserved for breaking changes that real downstream code has to react to.
- [ ] Should `embed.workers` be exposed on the CLI as `--workers N` for ad-hoc tuning, or live only in `~/.config/cortex/cortex.yml`? **Default answer:** config-only for now, matching the project's "config defines WHAT, CLI defines WHETHER" rule from `~/.claude/rules/general.md`. Adding a CLI flag is one line and reversible.

## Architect Consultation Summary

Two-round review with Gemini's Architect persona on 2026-05-17. Consensus reached across all findings.

### Findings resolved

| # | Finding (Round 1)                                                       | Disposition                                                                                                      |
|---|-------------------------------------------------------------------------|------------------------------------------------------------------------------------------------------------------|
| A | `ModelPool` in `cortex::embed` breaks abstraction and oversubscribes ONNX for the fastembed fallback | Resolved. Phase 6 redesigned: parallelism lives inside `CandleBertModel::embed_batch`. `cortex::embed` stays backend-agnostic. |
| B | Parallel write-back violates Phase A5's 200 ms / 64-row transaction discipline | Resolved. The 3-phase loop shape in `cortex/src/embed.rs` is unchanged; each batch still flushes in one short tx. |
| C | `BertModel: Clone` is unverified at candle 0.10.2 — RAM blow-up if cloning duplicates weights | Resolved in Phase 2 implementation notes: mmap-dedup fallback documented (load N times via `VarBuilder::from_mmaped_safetensors`, OS page cache shares the physical pages). Implementer verifies via `ps aux` RSS sampling. |
| D | fastembed auto-prepends `Represent this sentence for searching relevant passages:` for BGE queries; Candle omitting it causes drift | **Architect conceded.** Verified empirically against `fastembed-5.13.4/src/text_embedding/impl.rs:447` and `lib.rs:38-43`: fastembed tokenizes inputs verbatim, the caller (us) decides whether to prepend. Our existing `FastEmbedModel` adds no prefix. Candle adding no prefix preserves 1:1 parity. |
| E | Attention mask construction not explicit in Phase 2 — silent corruption hazard for padded batches | Resolved in Phase 2 notes: `attention_mask = input_ids.ne(pad_token_id)?.to_dtype(DType::U32)?`, passed to `BertModel::forward` as `Some(&attention_mask)`. |
| F | Rayon pool saturation stalls concurrent sweeps (autotag / quality / migrate / audit) | Resolved with caveat: cortex daemon serialises these sweeps via its `tokio::select!` loop, so the practical impact is "sweep triggered mid-backfill waits ~35-45 sec", not deadlock. Risks table notes the dedicated `ThreadPoolBuilder` escape hatch if the daemon shape ever runs sweeps concurrently with embed. |
| G | Hardest question: why does Phase 6 leak backend-specific thread pooling into `cortex::embed`? | Resolved. The redesigned Phase 6 places all concurrency behind `EmbeddingModel::embed_batch`. Architect confirmed Round 2: "abstraction boundary is fully restored. No further architectural concerns. Ready for implementation." |

### Design decisions confirmed by consultation

- **No query-side prefix.** Asymmetric prefixes between corpus and query would themselves cause distribution drift. Stay symmetric, raw-text; the BGE v1.5 model card confirms this is fine.
- **Pool sits inside the trait impl, not at the call site.** Tier 3 (fastembed) keeps single-mutex shape; its ONNX MLAS handles threading internally.
- **3-phase loop in `cortex::embed` is sacrosanct.** Any future perf work that touches it must re-validate the Phase A5 transaction-discipline regression test.

### Architect verdict

> "It is ready for implementation."

## Status

**Status:** Implemented

Phases 1-6 landed; Phase 7 (deploy + backfill) is operational and runs
on the live workstation - bump to v0.7.0, push, `otto deploy`, then
`cortex embed --prefetch-model && cortex embed --backfill`.

## References

- Doc 2: `docs/design/2026-05-16-hybrid-retrieval-fts5-vector-rrf.md`
- BGE model card: https://huggingface.co/BAAI/bge-small-en-v1.5
- Candle BERT example: https://github.com/huggingface/candle/tree/main/candle-examples/examples/bert
- Candle CPU module gating: `candle-core/src/cpu/mod.rs` (verified at v0.10.2)
- Pyke ONNX distribution table: `ort-sys-2.0.0-rc.12/build/download/dist.txt`
- Phase B post-review fixes: commits `8016c27` (NoteType taxonomy + video transcript + bench layout), `67931cc` (fmt)
- v0.6.1 ship + SIGILL discovery: this session's `cortex embed --backfill` exit 132 (signal 4)
