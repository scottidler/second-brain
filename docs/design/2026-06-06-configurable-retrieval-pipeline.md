# Design Document: Configurable Retrieval Pipeline for Oracle

**Author:** Scott Idler
**Date:** 2026-06-06
**Status:** Implemented
**Review Passes Completed:** 5/5 + architect design review (Gemini, 2026-06-06)

## Summary

Oracle's `knowledge_search` currently dispatches on a single, per-call `SearchMode`
(`bm25 | vector | hybrid | graph | graph-hybrid`) with hardcoded fusion constants and
no operator-facing retrieval config. This doc proposes a **configurable retrieval
pipeline** declared in `~/.config/sb/oracle.yml`: the operator describes the *shape* of
the pipeline (which retrievers compose, how they fuse, whether a rerank stage or a
query-transform stage exists), and oracle composes that pipeline for every query that
arrives without an explicit `mode`. The per-call MCP `mode` parameter stays as the
override. The whole design is anchored by one hard constraint - the daemon host is a
~2012 AVX-only Xeon - and one empirical finding - on this vault, pure `vector` beats
`hybrid`, so the shipped default flips to vector-first with BM25/graph demoted out of
the default fusion.

## Problem Statement

### Background

Oracle is the knowledge-retrieval MCP server in the second-brain workspace. Its
retrieval surface today:

- `oracle/src/tools.rs` - `SearchMode { Bm25, Vector, Hybrid, Graph, GraphHybrid }`,
  exposed as the per-call `mode` parameter on the `knowledge_search` MCP tool. Default
  `hybrid`.
- `oracle/src/server.rs::run_search_mode` - a single `match mode { ... }` that runs
  exactly one retriever path per call.
- `vault/src/search/vector.rs` - `reciprocal_rank_fusion(lists, RRF_K, limit)` with
  `pub const RRF_K: usize = 60` and `pub const K_RRF_INPUT: u32 = 50` (the per-retriever
  candidate depth fed into fusion). Fusion is unweighted: every list contributes equally.
- `oracle/src/server.rs` - graph expansion constants `MAX_EXPAND_HOPS: u8 = 2`,
  `GRAPH_HOP_DECAY: f32 = 0.5`.
- `vault/src/embedding.rs` - query embedding via `embed_query`, pinned to
  `bge-small-en-v1.5` (384-dim, L2-normalized). Backend is **Candle by default**,
  **fastembed (ONNX Runtime) behind the `vec-fastembed` feature flag**. Brute-force
  cosine over `note_embeddings` BLOB rows in SQLite. The active model is pinned in the
  `embedding_config` table so cortex (the only writer) and oracle cannot drift.
- `oracle/src/config.rs` - `Config { vault, db_path, logging, watcher,
  inbound_recompute_interval_secs }`. **No retrieval configuration exists.**

The prior eval work (`sb oracle eval`, nDCG@10 on the calibrated query set) measured:

| mode | nDCG@10 |
|------|---------|
| vector | **0.876** |
| hybrid | 0.799 |
| graph-hybrid | 0.572 |
| graph | 0.505 |
| bm25 | 0.268 |

Two findings drive this design:

1. **`hybrid` (0.799) is strictly worse than pure `vector` (0.876).** Fusing the weak
   BM25 list (0.268) into the strong vector list via equal-weight RRF *dilutes* the best
   signal. The default mode is actively costing retrieval quality.
2. **Graph adds noise, not lift.** Densifying the wikilink graph (+221 edges across 158
   notes) moved retrieval by -0.009 nDCG (within judge noise). Graph retrieval earns its
   keep for browsing and reasoning, not top-k ranking.

A verified third fact shapes which external techniques apply: cortex embeds the note's
`## Summary` text only (`EmbeddingKind::Summary` in `cortex/src/embed.rs`), not the raw
body. Notes are therefore already "contextualized" before embedding, so Anthropic's
Contextual Retrieval chunk-prefixing technique is mostly a no-op here (see Methodology 7).

### Problem

There is no way to change *which* retrieval methods oracle composes without editing Rust
constants and rebuilding. The one knob that exists - the per-call `mode` - is a single
mutually-exclusive selector, so an operator cannot say "retrieve with vector, then rerank
the top 50 with a cross-encoder," nor "fuse vector and bm25 but weight vector 3:1," nor
"drop stub notes from results." The eval has already shown the shipped default is
suboptimal, and there is no clean path to ship a better default or to let the operator
tune the pipeline against `sb oracle eval`.

### Goals

- Expose the retrieval pipeline's **composition** in `~/.config/sb/oracle.yml`: which
  retrievers run, how they fuse, whether a rerank stage exists, whether a query-transform
  stage exists, and which structural result filters apply.
- Allow **one or more** retrieval methods to compose into a single query, in a defined
  stage order.
- Keep the per-call MCP `mode` parameter as an **override** (precedence: per-call `mode`
  > configured pipeline > built-in default).
- Flip the built-in/shipped default to the **eval-best** shape: vector-first, BM25/graph
  demoted out of default fusion.
- Make a **cross-encoder rerank** stage available, off by default, latency-budgeted so it
  can never blow the interactive budget on the AVX-only host.
- Make **query transforms** (HyDE, multi-query) available, off by default.
- Keep the whole pipeline measurable through `sb oracle eval`.
- Expose methodology selection in the yml **directly and explicitly**: each retrieval
  method and each pipeline stage carries an `enabled` flag the operator flips, plus its
  tuning params. One or more methods can be enabled and they compose into the query.
  This **intentionally overrides** the general.md guidance "config defines WHAT rules look
  like, not WHETHER they run" - per explicit operator direction, selecting which retrieval
  methodology is active IS this config's job. general.md is amended with a carve-out (see
  Alternatives / References) so this doc is not in tension with it.

### Non-Goals

- **No ANN / SIMD-accelerated / GPU / newer-hardware retrieval libraries.** Brute-force
  cosine over SQLite BLOBs stays. The corpus is ~2,200 notes; brute force is not the
  bottleneck and the host has no fast inference path (see Performance).
- **The embedding model upgrade ships, but is not forced as a silent default.** The doc
  covers the bge-small -> bge-base / nomic-embed upgrade as a real, eval-gated lever
  (Methodology 7, Phase 7) - it is a cortex-side re-embed, pinned in `embedding_config`,
  measured with `sb oracle eval`. The "non-goal" is only that the *default* model pin does
  not flip blindly; the upgrade is implemented and made cheap to A/B, not deferred.
- **No new persistent store.** Rerank and query-transform stages are read-path compute;
  they add no tables and no second writer (cortex remains the only embeddings writer).
- **No removal of the legacy `SearchMode` enum.** It remains the per-call override surface
  and the unit of `sb oracle eval`'s mode sweep.

## Proposed Solution

### Overview

Introduce a `retrieval:` section in `oracle.yml` that declares an ordered pipeline:

```
query-transform?  ->  retrieve (1..N)  ->  fuse  ->  rerank?  ->  exclude-filters  ->  truncate
```

- **`enabled` flags select methodologies.** Each retrieval method (`vector`, `bm25`,
  `graph`) and each optional stage (`rerank`, `query-transform`) carries an `enabled`
  boolean. Flip it on to put that methodology in the pipeline; off to leave it out. One or
  more methods can be enabled at once; enabled retrievers fuse, enabled stages run in the
  fixed stage order. This is the operator's single answer to "which methodologies does
  oracle use for my queries."
- **Per-method tuning lives next to its flag.** `top-k`, fusion `weight`, graph `hops`,
  rerank `input-k` / `latency-budget-ms`, transform `variants` - each sits inside the
  block it tunes, so the whole pipeline is one readable section.
- **Fusion weights demote without disabling.** A method can be `enabled: true` but
  weighted low (bm25 `weight: 0.3`) or zero (graph `weight: 0.0`), so it stays available
  and eval-testable while vector dominates the ranking.
- **`mode` overrides everything.** When a caller passes an explicit `mode`, oracle runs
  that single legacy path and ignores the configured pipeline. This preserves back-compat
  and keeps `sb oracle eval`'s per-mode sweep meaningful.

### Recommendation (tiered)

The configurable pipeline is the vehicle; this is the opinion it encodes. Ranked by ROI on
*this* vault and *this* host:

**Tier 1 - highest ROI, the shipped default:**

- **A. Cross-encoder rerank stage (available, opt-in).** The single best-documented lever:
  cross-encoders add roughly +5-15 nDCG@10 in the literature, and Anthropic measured
  contextual-retrieval + reranking at 67% fewer retrieval failures (reranking the larger
  half). Purely additive - a second stage over the top-50 fused candidates returning a
  precision-tuned top-k; cortex/embeddings untouched. On this AVX-only box the model is a
  small `ms-marco-MiniLM-L6-v2` (22M), not the 278M `bge-reranker-v2-m3`, and it is
  latency-budgeted with a fail-open probe (Performance). Off by default, one flag to enable.
- **B. Fix the fusion inversion (the new default).** Eval shows `hybrid` (0.799) loses to
  pure `vector` (0.876): equal-weight RRF dilutes the strong signal. The shipped default
  enables `vector` only, with `bm25` (weight 0.3) and `graph` (weight 0.0) demoted and off.
  Near-zero-risk, A/B-able with `sb oracle eval` on day one.

**Tier 2 - worth testing (implemented, eval-gated):**

- **C. Embedding-model upgrade.** `bge-small-en-v1.5` is 2023-era / 384-dim. Brute-force
  cosine over ~2,200 notes is negligible even at 768-dim, so dimension is not the limit -
  the AVX-only inference budget is. Phase 7b evaluates `bge-base-en-v1.5` /
  `nomic-embed-text-v2`, re-pins in `embedding_config`, and gates the switch on a measured
  lift over 0.876.

**Tier 3 - deprioritized for retrieval (available, off by default):**

- **Query expansion / HyDE** - solves recall, which is not the bottleneck here; adds an LLM
  round-trip and can poison precision. Implemented as an opt-in stage, off by default.
- **Anthropic chunk-prefixing context** - largely already solved: cortex embeds distilled
  `## Summary` text, not blind chunks, so the 35% gain mostly does not apply. The cheap
  residual (prepend the note *title* to the summary before embedding) is Phase 7a.
- **Graph as a retrieval contributor** - keep the graph for browsing/reasoning; demote it
  out of the default fused result (weight 0.0) so it stops diluting vector. Same as lever B.

**Bottom line:** the best system for this vault and host is **vector-first retrieval + an
optional lightweight cross-encoder reranker, with BM25 and graph demoted out of the default
fusion.** Every step is provable with the existing `sb oracle eval` harness before it ships.

### Architecture

Components and how they change:

| Component | Change |
|-----------|--------|
| `oracle/src/config.rs` | Add `RetrievalConfig` (and nested `RetrieverSpec`, `FusionConfig`, `RerankConfig`, `QueryTransformConfig`, `ExcludeFilter`) to `Config`, kebab-case serde, all `#[serde(default)]`. |
| `oracle/src/tools.rs` | `SearchMode` unchanged. The `knowledge_search` `mode` param is **already** `Option<SearchMode>` (no schema change). The change is purely *behavioral*: when `mode` is `None`, route to `run_pipeline` instead of the current default-to-`Hybrid`. `Some(_)` stays the legacy single-mode override. |
| `oracle/src/server.rs` | `run_search_mode` keeps the legacy `match` for the override path. Add `run_pipeline(config.retrieval, query, filters, limit)` that composes stages, **including the rerank call and the query-transform call** (transform lives here, not in `vault` - see below). The two share the retriever primitives. |
| `vault/src/search` | `reciprocal_rank_fusion` gains an overload (or new fn) taking per-list weights and a runtime `k`. Add a `rerank` module (cross-encoder scoring - **local model inference, mirrors `vault::embedding`, no HTTP**). `RRF_K`/`K_RRF_INPUT` remain as the *defaults* the config falls back to. **Query transforms do NOT live in `vault`** (see Architecture note below). |
| `config/templates/oracle.yml` | Ship a documented `retrieval:` section set to the eval-best default. |
| `sb bootstrap` | Already drops `oracle.yml` from the template; no new logic, the template just grows the section. |
| `oracle/src/eval.rs` | Add a `configured` eval target that runs `run_pipeline` so the operator can measure the live pipeline, not only the 5 legacy modes. |

The retriever primitives (BM25 query, vector query, graph expansion) are already factored
inside `run_search_mode` / `graph_dispatch`. The refactor lifts each into a private helper
returning a ranked `Vec<String>` (note paths), so both the legacy `match` and the new
`run_pipeline` call the same code. No retrieval logic is duplicated.

**Crate-layering rule (corrected per architect review):** `vault` is the core library;
`borg` depends on `vault`, not the reverse, and `vault` has **no HTTP client** (`reqwest`
lives in `borg`). Therefore the two inference-bearing stages land in different crates:

- **Rerank stays in `vault::search::rerank`.** A cross-encoder is *local model inference*,
  exactly like `vault::embedding::embed_query` already is - no network, no new crate
  dependency beyond the ML backend `vault` already links.
- **Query transforms (HyDE / multi-query) live in `oracle::run_pipeline`, NOT `vault`.**
  They issue an LLM call, which needs an HTTP client / the fabric port. Putting that in
  `vault` would create a circular dependency or bloat the core crate. Oracle (the
  composition point) owns the transform stage and passes the rewritten query/embedding
  down into the `vault` retriever primitives.

### Data Model

The new `oracle.yml` section. All keys kebab-case; every field defaulted so an existing
config without a `retrieval:` block still loads (and gets the eval-best built-in default).

```yaml
retrieval:
  # The pipeline oracle composes when a query arrives with no explicit `mode`.
  # Stage order is fixed: query-transform -> retrieve -> fuse -> rerank -> exclude -> truncate.
  # Each method/stage has an `enabled` flag: flip it on to put that methodology in the
  # pipeline. One or more retrievers may be enabled; enabled retrievers fuse via RRF.

  # --- 1. Retrievers. Enable one or more. Each enabled method yields a ranked list. ---
  methods:
    vector:
      enabled: true        # the eval-best retriever for this host; on by default
      top-k: 50            # candidate depth fed into fusion
    bm25:
      enabled: false       # exact-keyword retriever; off by default (dilutes vector)
      top-k: 50
      weight: 0.3          # fusion weight if enabled (demoted relative to vector)
    graph:
      enabled: false       # wikilink-expansion retriever; off by default (no ranking lift)
      top-k: 50
      weight: 0.0          # demoted out of the fused result even if enabled
      hops: 2              # falls back to MAX_EXPAND_HOPS
      hop-decay: 0.5       # falls back to GRAPH_HOP_DECAY
      min-edge-weight: 0.0
      edge-kinds: [wikilink]

  # --- 2. Fusion. Consulted only when >1 retriever is enabled. ---
  # Weighted RRF: each enabled retriever's reciprocal-rank contribution is scaled by the
  # `weight` declared in its block above (a method with no `weight` defaults to 1.0).
  fusion:
    method: rrf
    k: 60                  # falls back to RRF_K

  # --- 3. Rerank. Optional cross-encoder second stage. ---
  # OFF by default on this host: cross-encoder inference is the most AVX-sensitive stage.
  rerank:
    enabled: false
    method: cross-encoder
    model: ms-marco-MiniLM-L6-v2   # small (~22M); the only CPU-sane default
    input-k: 50                     # rerank the top 50 fused candidates
    latency-budget-ms: 1500         # warmup probe; if exceeded, stage no-ops (fail-open)

  # --- 4. Query transform. Optional pre-retrieval rewrite. ---
  # OFF by default: adds an LLM round-trip per query and can poison precision.
  query-transform:
    enabled: false
    method: hyde                    # hyde | multi-query
    model: <local-llm-endpoint-or-fabric-pattern>
    variants: 3                     # multi-query only: number of rewrites

  # --- 5. Exclude filters. Structural result-shape filters, applied post-fusion. ---
  exclude:
    stub: true             # drop low-content hub/stub notes from results
    min-body-chars: 0      # 0 = off; >0 drops notes with shorter bodies
```

Rust types (sketch; `#[serde(rename_all = "kebab-case")]`, all `#[serde(default)]`):

```rust
pub struct RetrievalConfig {
    pub methods: MethodsConfig,             // per-retriever enabled + tuning
    pub fusion: FusionConfig,               // rrf k (default 60)
    pub rerank: RerankConfig,               // enabled=false by default
    pub query_transform: QueryTransformConfig, // enabled=false by default
    pub exclude: ExcludeConfig,             // stub=true, min-body-chars=0 by default
}

pub struct MethodsConfig {
    pub vector: VectorMethod,               // enabled=true, top-k=50
    pub bm25: Bm25Method,                    // enabled=false, top-k=50, weight=0.3
    pub graph: GraphMethod,                  // enabled=false, weight=0.0, hops=2, ...
}

// Each method carries its own `enabled: bool`, `top_k: u32`, and (bm25/graph) `weight: f32`.
// GraphMethod additionally carries hops/hop_decay/min_edge_weight/edge_kinds.

pub struct FusionConfig {
    pub method: FusionMethod,               // Rrf (only variant today; extensible)
    pub k: usize,                           // default RRF_K (60)
}

pub struct RerankConfig {
    pub enabled: bool,                       // default false
    pub method: RerankMethod,                // CrossEncoder
    pub model: String,                       // default "ms-marco-MiniLM-L6-v2"
    pub input_k: u32,                        // default 50
    pub latency_budget_ms: u64,              // default 1500
}

pub struct QueryTransformConfig {
    pub enabled: bool,                       // default false
    pub method: TransformMethod,             // Hyde | MultiQuery
    pub model: String,
    pub variants: u8,                        // default 3 (multi-query)
}

pub struct ExcludeConfig {
    pub stub: bool,                          // default true
    pub min_body_chars: usize,               // default 0 (off)
}
```

The weighted-RRF primitive reads each enabled method's `weight`; `enabled: false` methods
are never queried, so they contribute nothing regardless of weight.

### API Design

- **MCP surface: no schema change.** `knowledge_search`'s `mode` is *already*
  `Option<SearchMode>` in `oracle/src/tools.rs`; this work does not touch the schema. The
  change is purely behavioral: `Some(mode)` -> legacy single-mode path (`run_search_mode`),
  unchanged; `None` -> `run_pipeline` (today `None` defaults to `Hybrid`). Callers that
  pass an explicit `mode` are entirely unaffected. **The one observable behavior change:**
  callers that rely on `mode: None` silently yielding hybrid results will get the
  vector-first configured pipeline instead. This is the intended default flip (see Risks).

- **New internal entry point:**

  ```rust
  // oracle/src/server.rs
  pub fn run_pipeline(
      &self,
      db: &SearchIndex,
      cfg: &RetrievalConfig,
      query: &str,
      domain: Option<&str>,
      note_type: Option<&str>,
      status: Option<&str>,
      limit: u32,
  ) -> Result<Vec<NoteRow>, McpError>;
  ```

  Stage flow inside `run_pipeline` (oracle owns this orchestration; transform and rerank
  are gated by their `enabled` flags):
  1. **Transform.** If `cfg.query_transform.enabled`, oracle rewrites/expands `query`
     **here in the oracle crate** (HyDE replaces the embedding input with a
     hypothetical-answer embedding; multi-query issues N retrievals and unions candidates).
     This is the LLM-bearing stage and must NOT live in `vault` (no HTTP client there).
     Logged at DEBUG with the transform method.
  2. **Retrieve.** For each `enabled` method, call the shared `vault` primitive, collecting
     a ranked `Vec<String>` per method, each truncated to its `top-k`. The vector primitive
     **calls `warn_if_no_embeddings` internally, before truncating the `Vec<VectorHit>`
     down to `Vec<String>`** - the warning needs the hit structs, which are dropped at the
     truncation boundary, so it cannot be hoisted into the caller.
  3. **Fuse.** If one method enabled, pass through. If more, weighted RRF with `cfg.fusion.k`
     and each method's `weight`.
  4. **Rerank.** If `cfg.rerank.enabled`, score the top `input-k` fused candidates with the
     `vault::search::rerank` cross-encoder, re-order by score. Latency-budgeted (Performance).
  5. **Exclude.** Apply `cfg.exclude` filters: `stub` drops notes whose `quality` column is
     `low` (see Phase 3 - that is the only stubness signal actually in the `notes` table);
     `min-body-chars` drops short bodies.
  6. **Truncate** to `limit`, resolve to `NoteRow`s.

- **New weighted-fusion primitive:**

  ```rust
  // vault/src/search/vector.rs (or a new fuse.rs)
  pub fn reciprocal_rank_fusion_weighted(
      lists: &[(&[String], f32)],   // (ranked paths, weight)
      k: usize,
      limit: usize,
  ) -> Vec<Fused>;
  ```

  The existing unweighted `reciprocal_rank_fusion` becomes a thin wrapper that calls this
  with all weights 1.0, so no caller breaks.

### Implementation Plan

All phases are a single build sequence to execute back-to-back. There is no soak period,
no evidence gate between phases, and no deferred follow-up; every methodology named in the
goals is implemented in this sequence.

#### Phase 1: Config types + weighted fusion primitive
**Model:** sonnet
- Add `RetrievalConfig` and nested types to `oracle/src/config.rs` with kebab-case serde
  and `#[serde(default)]` on every field; default = vector-only, rrf k=60, weights
  `{vector:1, bm25:0.3, graph:0}`, no rerank, no transform, exclude `[stub]`.
- Add `config/tests.rs` cases: empty config loads to the eval-best default; a fully
  specified `retrieval:` block round-trips; unknown keys error.
- Add `reciprocal_rank_fusion_weighted` to `vault::search`; make the existing
  `reciprocal_rank_fusion` delegate with uniform weights. Unit-test that uniform weights
  reproduce the current ordering exactly.

#### Phase 2: Refactor retrievers into shared primitives + run_pipeline
**Model:** opus
- Lift the BM25, vector, and graph-expansion bodies out of `run_search_mode` /
  `graph_dispatch` into private helpers returning `Vec<String>` (ranked paths). Keep
  `run_search_mode` working by calling them (pure refactor; existing tests must stay green).
- Implement `run_pipeline` for the retrieve -> fuse -> exclude -> truncate stages (rerank
  and transform wired as no-ops here, filled in Phase 4-5).
- The vector primitive must call `warn_if_no_embeddings(db, &hits)` **before** it truncates
  `Vec<VectorHit>` to `Vec<String>`; the warning consumes the hit structs and cannot be
  done by the caller after truncation.
- The `knowledge_search` `mode` param is already `Option<SearchMode>` - no signature
  change. Only change the `None` branch: route to `run_pipeline` instead of defaulting to
  `Hybrid`. Default the built-in (no config) pipeline to the eval-best shape.

#### Phase 3: Exclude filters (stub + min-body-chars)
**Model:** sonnet
- `ExcludeFilter::Stub` drops notes whose `quality` column is `low`. **Architect-verified
  constraint:** cortex's richer `cortex-quality-issues` frontmatter (which carries the
  `[stub-body]` marker, written in `cortex/src/quality.rs`) is NOT a column in the `notes`
  SQLite table - only `quality` (`low`/`medium`/`high`) is queryable. Post-fusion exclusion
  runs against the DB rows, so use the `quality` column; do not assume the `[stub-body]`
  marker is reachable without a live file read. `MinBodyChars` uses the retrieved body
  length. Apply post-fusion in `run_pipeline`.
- Tests: a `quality=low` note in the fused list is dropped; a `quality=high` note survives;
  `min-body-chars` drops a short body.

#### Phase 4: Cross-encoder rerank stage (off by default)
**Model:** opus
- Add `vault::search::rerank` with a Candle cross-encoder scorer. **Architect-verified
  reality, not "backend reuse":** `fastembed` is unavailable on the daemon host - its
  bundled ONNX Runtime aborts at startup without AVX2 (`vault/src/embedding/fastembed.rs`
  guards on `!is_x86_feature_detected!("avx2")`), so the daemon must compile `vec-candle`.
  Candle today only does single-text CLS-pooled embeddings (`CandleBertModel`), so a
  cross-encoder is a **new pipeline to build** in this crate: token-PAIR sequencing
  (query + candidate as one input with segment ids) plus a classification/regression head,
  not a call into an existing rerank API. Budget honestly (Performance): scalar Candle is
  ~150-300 ms per pair, so 50 unbatched pairs is ~7.5-15 s - the stage is only viable with
  rayon intra-batch parallelism across the 32 threads, and even then the budget probe will
  often trip on this host. No HTTP; this is local inference, so it correctly lives in `vault`.
- Wire `RerankConfig` into `run_pipeline` stage 4 (oracle-side gating on `enabled`).
  Implement the warmup latency probe: on first use, time one query-doc pair; if the
  projected `input-k` batch (accounting for the rayon thread count) exceeds
  `latency-budget-ms`, log WARN and no-op the stage for the process (fail-open to fused
  order). This is the AVX-only safety valve and is expected to fire frequently.
- Tests: rerank reorders a known-relevant doc to the top on a fixture; budget-exceeded
  path falls back to fused order and logs WARN.

#### Phase 5: Query transforms (HyDE + multi-query, off by default)
**Model:** opus
- Add the transform stage **in `oracle` (`oracle/src/server.rs` / a new `oracle` module),
  NOT in `vault`.** Architect-verified layering: `vault` is the core crate, has no
  `reqwest`/HTTP client, and `borg` depends on `vault` (not the reverse); an LLM call from
  inside `vault::search` would be a circular dependency or core-crate bloat. Oracle owns
  the LLM round-trip (via a local fabric pattern / configured endpoint) and passes the
  rewritten query or hypothetical-answer embedding *down* into the `vault` retriever
  primitives. HyDE: generate a hypothetical answer, embed *that* instead of the raw query.
  Multi-query: generate N rewrites, retrieve each, union candidates before fusion.
- Wire `QueryTransformConfig` into `run_pipeline` stage 1.
- Tests: HyDE path embeds the generated text (mock the model); multi-query unions N
  candidate lists.

#### Phase 6: Template, eval target, docs
**Model:** sonnet
- Grow `config/templates/oracle.yml` with the documented `retrieval:` section at the
  eval-best default (vector-only, stub-exclude, rerank/transform commented out with
  guidance).
- Add the `configured` target to `oracle/src/eval.rs` so `sb oracle eval` can score the
  live pipeline alongside the 5 legacy modes.
- Update `oracle/AGENTS.md` and the root `CLAUDE.md` "Hybrid retrieval (Doc 2)" section to
  describe the configurable pipeline and the default change.

#### Phase 7: Embedding-input refinement + model upgrade (cortex-side, eval-gated)
**Model:** opus
- **7a. Title+summary prefix.** In `cortex/src/embed.rs::process_summary_batch`, change the
  embedded text from `t.summary` to `format!("{title}\n\n{summary}")` (title carries strong
  topical signal). Same model pin and dimension. Requires a full re-embed
  (`sb cortex embed --backfill`). Measure with `sb oracle eval` before/after.
- **7b. Embedding-model upgrade.** Evaluate replacing `bge-small-en-v1.5` (384-dim, 2023)
  with a stronger CPU-runnable model: `bge-base-en-v1.5` (768-dim) or `nomic-embed-text-v2`
  (137M, Matryoshka 768->64). Brute-force cosine over ~2,200 notes is negligible even at
  768-dim, so dimension is not the constraint; model inference cost on the AVX-only host is
  - so prefetch and benchmark the candidate's embed throughput on `desk` before committing.
  Re-pin in the `embedding_config` table (the single source of truth oracle and cortex both
  read), full re-embed, and gate the switch on a measured `sb oracle eval` lift over the
  current 0.876 vector baseline. If no candidate beats it within the CPU budget, keep
  bge-small - the upgrade is eval-gated, not assumed.

## Alternatives Considered

### Alternative 1: Add more `SearchMode` enum variants
- **Description:** Keep the single-`mode` dispatch; add variants like `vector-rerank`,
  `vector-hyde`, `hybrid-weighted`.
- **Pros:** Smallest change; no config schema.
- **Cons:** Combinatorial explosion (every retriever x fusion x rerank x transform
  combination is a new variant); no operator tuning without a rebuild; weights and budgets
  still hardcoded. Does not satisfy "one or more methods configurable in yml."
- **Why not chosen:** The goal is operator-configurable composition, which an enum cannot
  express.

### Alternative 2: Per-call pipeline in the MCP request
- **Description:** Let the caller pass the full pipeline spec as MCP tool arguments on
  every `knowledge_search` call.
- **Pros:** Maximum flexibility; no config file.
- **Cons:** Pushes pipeline design onto every caller/agent; no single source of truth for
  "how this vault retrieves"; the operator (Scott) cannot set a house default; clutters the
  tool schema.
- **Why not chosen:** The operator wants a *configured* house pipeline. Per-call override
  via `mode` is retained for the narrow cases that need it.

### Alternative 3: presence-of-block (no `enabled` flags)
- **Description:** Model the pipeline as shape - a stage block present in config runs, an
  absent block does not - to avoid bare `enabled: true/false`, per general.md's "config
  defines WHAT rules look like, not WHETHER they run."
- **Pros:** Stays inside the letter of the existing general.md rule without amending it.
- **Cons:** Indirect. "Is bm25 on?" becomes "is there a bm25 entry?" The operator's
  mental model is "turn methodologies on and off," and the explicit `enabled` flag says
  exactly that. Commenting blocks in and out is a clumsier on/off than a boolean.
- **Why not chosen:** **Explicit operator direction.** The whole point of this feature is
  to let the operator select which methodologies oracle uses; an explicit `enabled` flag
  is the clearest expression of that. general.md is amended with a carve-out (selecting
  which algorithm/methodology is *active* is legitimate config, distinct from gating
  whether a fixed governance rule *runs*) so the adopted design is consistent with the
  rules, not in violation of them.

### Alternative 4: Drop graph/bm25 entirely; ship vector-only
- **Description:** The eval says vector wins, so delete the other retrievers.
- **Pros:** Simplest possible system; no fusion needed.
- **Cons:** Throws away BM25's exact-keyword strength (the eval query set is calibrated but
  not exhaustive; lexical queries like exact error strings or rare proper nouns are where
  BM25 wins and vector misses), and graph's browsing value. Irreversible; no way to A/B.
- **Why not chosen:** Demotion via fusion weight (graph 0.0, bm25 0.3) keeps the
  capability available and tunable while making vector dominant. Configurability is the
  point; deletion forecloses it.

## Technical Considerations

### Dependencies

- **No new retrieval store, no new writer.** Cortex remains the only `note_embeddings`
  writer.
- **Rerank inference (Candle only on this host; it is a build, not "reuse"):** the
  architect verified that `fastembed`'s bundled ONNX Runtime *aborts at startup without
  AVX2* (`vault/src/embedding/fastembed.rs` guards on `is_x86_feature_detected!("avx2")`),
  so the daemon host cannot use the `vec-fastembed` `TextRerank` path at all - it must run
  `vec-candle`. Candle currently does only single-text CLS-pooled embeddings; a
  cross-encoder requires implementing token-pair sequencing + a classification head in
  `vault::search::rerank`. This adds no new *crate* dependency (Candle is already linked),
  but it is genuine new ML-pipeline code, not a call into an existing rerank API. Model
  weights (`ms-marco-MiniLM-L6-v2`) download on first use into the Candle/HF cache; add a
  prefetch step so the daemon never blocks on a cold fetch.
- **Query-transform inference (lives in `oracle`, not `vault`):** an LLM round-trip via a
  local fabric pattern / configured endpoint. `vault` has no HTTP client and `borg` depends
  on `vault`, so the call cannot originate in `vault`; oracle owns it. Slowest possible
  stage; off by default.

### Performance

This is the section the whole design is anchored to.

**Host:** `desk` - dual-socket Intel Xeon E5-2600 (Sandy Bridge-EP, family 6 model 45),
16 cores / 32 threads @ 3.1 GHz, 94 GiB RAM. **SIMD: AVX + SSE4.1/4.2 only - no AVX2, no
FMA, no AVX-512, no VNNI.** Launched ~2012.

Implications, per stage:

- **Vector (brute-force cosine):** unaffected by the SIMD gap in practice. ~2,200 notes x
  384 dims is a trivial dot-product sweep; it is already the fast path and stays. Corpus
  growth to even 10x does not threaten the budget. This is *why* "no ANN" is a non-goal,
  not a regret.
- **BM25 (FTS5):** SQLite native, no inference, negligible.
- **Graph expansion:** bounded by `MAX_EXPAND_HOPS=2` and edge reads; no inference.
- **Cross-encoder rerank:** the AVX-sensitive stage, and the architect's harshest finding.
  fastembed cannot run here (it aborts without AVX2), so this is a Candle scalar-backend
  cross-encoder. Candle's per-call cost on this host is ~150-300 ms (the embedding path's
  own comments cite this), so **50 candidate pairs run *unbatched* is ~7.5-15 s** - far past
  any interactive budget. Viability rests entirely on parallelism + the safety valve:
  (a) off by default; (b) `input-k` caps the batch; (c) the warmup probe must project the
  batch cost *accounting for the rayon thread count* (32 threads over 50 pairs is ~2 waves)
  and no-op the stage if it exceeds `latency-budget-ms`, failing open to the fused order;
  (d) **expect the probe to trip frequently on this box** - that is the honest baseline, not
  an edge case. Larger rerankers (bge-reranker-v2-m3, 278M) are categorically off the table.
  This stage is shipped *enabled-able* per the operator's "all methodologies configurable"
  directive, but its real-world cost on `desk` is documented as marginal, not assumed cheap.
- **Query transform (HyDE/multi-query):** an LLM call (or N), latency dominated by the
  model endpoint, not the CPU. Off by default; documented as the highest-latency option.

**General rule the doc encodes:** every inference-bearing stage is off by default and
latency-budgeted, so the shipped pipeline on this host is exactly the stage that the
hardware runs for free (vector) plus zero-inference filtering. The operator opts into cost
explicitly, with a safety valve that degrades gracefully rather than hanging a query.

### Security

- No new external network surface beyond model-weight fetch on first use (same trust
  boundary as the existing embedding model). Query-transform endpoints are operator-
  configured; document that an external LLM endpoint sends query text off-box (the local
  fabric path keeps it on-box).
- No new secrets. No new writers to either SQLite file (one-way data-flow invariant
  preserved).

### Testing Strategy

- **Unit:** config round-trip + defaults (`oracle/src/config/tests.rs`); weighted RRF
  equals unweighted under uniform weights; each stage in isolation (retriever helpers,
  weighted fusion, each exclude filter, rerank reorder, rerank budget fail-open, HyDE
  embed-substitution, multi-query union).
- **Integration:** `run_pipeline` end-to-end on a mini-vault fixture for representative
  shapes (vector-only; vector+bm25 weighted; vector+rerank; hyde+vector).
- **Eval (the real acceptance gate):** `sb oracle eval` with the new `configured` target.
  Acceptance for the default shape: the shipped vector-first default must score >= the
  current `vector` mode (0.876) and strictly > the current `hybrid` (0.799). Each opt-in
  stage is justified by an eval delta the operator can reproduce, not by assertion.

### Rollout Plan

- Ship the new `config/templates/oracle.yml` section. `sb bootstrap` drops it on fresh
  installs; existing installs without a `retrieval:` block transparently get the eval-best
  built-in default (the **intentional default change**, see Risks).
- Oracle is launched on demand via `.mcp.json -> sb oracle serve`; no daemon restart
  choreography. `otto deploy` installs the new `sb`; the next MCP launch picks up the new
  default.
- The rerank model (if the operator enables it) prefetches via the documented command so
  the first enabled query does not block on a download.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Default flips from `hybrid` to vector-first; an install relying on hybrid behavior changes silently | High | Med | Documented loudly in CLAUDE.md + template; eval shows the change is strictly better (0.876 vs 0.799); `mode: hybrid` still available per-call for exact back-compat |
| Cross-encoder rerank exceeds the latency budget on AVX-only host | **High** (architect: unbatched ~7.5-15 s; probe expected to trip often) | High | Off by default; `input-k` cap; warmup probe fail-open to fused order with WARN; rayon intra-batch parallelism; small model only; cost documented as marginal not cheap |
| Building a Candle cross-encoder is more work than "backend reuse" implied | **High** | Med | Phase 4 rescoped: explicitly a new token-pair + classification-head pipeline in `vault::search::rerank`; fastembed `TextRerank` is unavailable (aborts without AVX2), so there is no shortcut to acknowledge |
| Query-transform LLM call adds seconds and can poison precision | Med | Med | Off by default; documented precision-poisoning risk; eval-gated before any operator turns it on |
| Query transform wrongly placed in `vault` -> circular dep / core-crate HTTP bloat | (caught in review) | High | Resolved in design: transform lives in `oracle::run_pipeline`; only the rewritten query/embedding is passed into `vault` retrievers |
| Refactor of `run_search_mode` into shared primitives regresses legacy modes | Med | High | Pure refactor in Phase 2 gated by the existing mode tests staying green; weighted-RRF wrapper proven equivalent to unweighted under uniform weights; `warn_if_no_embeddings` called inside the vector primitive before truncation |
| `min-body-chars` / stub exclusion drops a legitimately short but relevant note | Low | Med | Stub uses the `quality=low` column (the only stub signal in the `notes` table); threshold is operator-tunable; the `exclude` filter is removable |

## Open Questions

- [x] **Resolved (architect):** Does cortex expose a stub signal `ExcludeFilter::Stub` can
      reuse? Cortex writes the `[stub-body]` marker into the `cortex-quality-issues`
      frontmatter (`cortex/src/quality.rs`), but that field is NOT a column in the `notes`
      SQLite table - only `quality` (`low`/`medium`/`high`) is. So stub exclusion drops
      `quality=low` rows; the richer marker is not reachable post-fusion without a file read.
- [x] **Resolved (architect):** Is the fastembed `TextRerank` path available on this host?
      No - fastembed's ONNX Runtime aborts without AVX2 (`vault/src/embedding/fastembed.rs`).
      The daemon must run `vec-candle`, and the cross-encoder is a from-scratch Candle build.
- [ ] Should `multi-query` union happen before or after per-retriever `top-k` truncation?
      (Leaning before, so each rewrite gets full depth; confirm against eval in Phase 5.)
- [ ] HyDE generation model: a dedicated fabric pattern, or the same model borg uses for
      distillation? (Operator choice; default to the on-box fabric path for privacy. Lives
      in `oracle`, not `vault`.)
- [ ] **Open (raised by review):** given the cross-encoder's ~7.5-15 s unbatched cost on
      this host, is rerank worth implementing now, or is the config hook plus the
      vector-first default flip the shippable core, with Phase 4 built but realistically
      never enabled on `desk`? (Operator call - the directive was to make every methodology
      configurable; this records the cost so the decision is informed.)

## References

- Prior eval baseline + densification result (this repo): `sb oracle eval` reports,
  `docs/design/2026-05-16-hybrid-retrieval-fts5-vector-rrf.md`, and the 2026-06-06 graph
  retrieval eval design.
- Anthropic, "Contextual Retrieval in AI Systems" - https://www.anthropic.com/news/contextual-retrieval
  (chunk-prefixing + rerank; mostly N/A here because cortex embeds distilled summaries).
- Cross-encoder reranking: BigData Boutique, "RAG Reranking: Improving Retrieval Quality
  with Cross-Encoders" - https://bigdataboutique.com/blog/rag-reranking-improving-retrieval-quality-with-cross-encoders
- Query transforms (HyDE / multi-query): Neo4j, "Advanced RAG Techniques" -
  https://neo4j.com/blog/genai/advanced-rag-techniques/
- Embedding-model landscape (CPU constraint context only; no model change in this doc):
  Milvus, "Best Embedding Models for RAG 2026" - https://milvus.io/blog/choose-embedding-model-rag-2026.md
- Code anchors: `oracle/src/tools.rs` (`SearchMode`), `oracle/src/server.rs`
  (`run_search_mode`, `graph_dispatch`), `vault/src/search/vector.rs` (`RRF_K=60`,
  `K_RRF_INPUT=50`, `reciprocal_rank_fusion`), `vault/src/embedding.rs` (bge-small pin,
  Candle/fastembed backends), `cortex/src/embed.rs` (`process_summary_batch`,
  `EmbeddingKind::Summary`).
