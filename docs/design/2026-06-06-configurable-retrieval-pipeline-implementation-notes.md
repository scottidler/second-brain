# Implementation Notes: Configurable Retrieval Pipeline

Running, append-only record of how the implementation interprets or diverges
from `docs/design/2026-06-06-configurable-retrieval-pipeline.md`. One section
per phase. See `how-to-execute-a-plan` skill for the format contract.

## Phase 1: Config types + weighted fusion primitive

### Design decisions
- `oracle/src/config.rs` — added `RetrievalConfig` + nested types inline in
  `config.rs` rather than a new `config/retrieval.rs` module. The file stays
  well under the 1500-line cap and the types are config, so they belong with
  the rest of the config surface.
- `deny_unknown_fields` on every retrieval struct (`RetrievalConfig`,
  `MethodsConfig`, and all 7 leaf method/stage structs) — the doc's Phase 1
  test list calls for "unknown keys error". This is hand-edited operator
  config, so a typo (`enabledd: true`) should fail loudly rather than be
  silently ignored. Enums are left without it (meaningless on unit variants).
- `default_rrf_k()` returns `vault::search::RRF_K` (not a hardcoded 60) so the
  config default tracks the vault constant the doc names as the fallback.
- `QueryTransformConfig` carries a `pattern: String` field (default `"hyde"`)
  in addition to `model` — Phase 5 invokes a Fabric *pattern* by name (mirrors
  `oracle::eval::FabricJudge`, which resolves a pattern under
  `~/.config/sb/patterns/`). The doc's Data Model sketch only showed `model`;
  `pattern` is the gap-fill needed to actually drive `vault::fabric::run_pattern`.
- `vault::search::reciprocal_rank_fusion_weighted` is the new weighted core;
  the existing `reciprocal_rank_fusion` is now a thin wrapper passing weight 1.0
  per list, so every existing caller is byte-identical (proven by
  `weighted_rrf_with_uniform_weights_equals_unweighted`).

### Deviations
- **Weighted RRF skips non-positive weights** (`if *weight <= 0.0 { continue }`)
  rather than adding a 0.0-score entry. The design says graph `weight: 0.0` is
  "demoted out of the fused result even if enabled." A literal `weight * contrib`
  with weight 0.0 still inserts the note at score 0.0, so a graph-only note
  would leak into results at the bottom by tiebreaker. Skipping makes 0.0 a true
  no-op (a note reachable *only* via a 0.0-weighted list never appears), which
  matches "out of the fused result." Caught by a unit test that initially failed.
  Uses `<= 0.0` (ordered comparison) deliberately to avoid the `clippy::float_cmp`
  lint that `== 0.0` would trip, and to also guard nonsensical negative weights.

### Tradeoffs
- Inline config types vs. a dedicated `config/retrieval.rs` module — chose inline
  for now; if the rerank/transform config grows, splitting is a clean later move.
- `pattern` + `model` split on `QueryTransformConfig` vs. a single field —
  split mirrors how `FabricJudge` already separates pattern name from model.

### Open questions
- None for Phase 1. (The doc's standing open questions about multi-query union
  ordering and the HyDE generation model are resolved in Phases 5.)

## Phase 2: Shared retriever primitives + run_pipeline

### Design decisions
- Retrievers lifted into **private methods on `OracleMcpServer`**
  (`bm25_paths`, `vector_paths`, `expand_to_graph_paths`) returning
  `Vec<String>`, per architect guidance - NOT free functions in `vault`. The
  only new `vault` symbol stays `reciprocal_rank_fusion_weighted` (Phase 1).
- `vector_paths` calls `warn_if_no_embeddings(db, &hits)` on the `VectorHit`
  slice *before* mapping to `Vec<String>`, so the zero-embeddings warning still
  fires (the hits are dropped at the path-map boundary).
- `expand_to_graph_paths` takes `hop_decay` as a parameter so legacy graph modes
  pass the built-in `GRAPH_HOP_DECAY` and the configured pipeline passes
  `cfg.methods.graph.hop_decay`. The scoring is otherwise byte-identical to the
  old inline `graph_dispatch` body.
- `run_pipeline` reports `mode: "configured"` in the response JSON (matches the
  Phase 6 eval target name). Explicit modes keep their existing labels.
- Stage order is laid out with the rerank (Phase 4) and exclude (Phase 3) and
  transform (Phase 5) insertion points marked by comments rather than dead
  no-op calls, so each phase's commit stays free of dead code.
- Fusion truncates to a `candidate_limit` (`max(limit, K_RRF_INPUT, input_k)`)
  not `limit`, so the later exclude/rerank stages have headroom and the result
  still fills `limit` after they run. The final `take(limit)` is the last step.

### Deviations
- **Graph retriever seeding in the pipeline is a gap-fill, not a deviation.**
  The doc's `graph:` method block specifies hops/decay/weight/edge-kinds but not
  how the graph retriever is *seeded*. `pipeline_graph_paths` seeds from the
  hybrid (bm25+vector) fused order - exactly how the legacy graph modes seed -
  then expands and caps at `top-k`. Consequence: when graph is enabled it
  re-runs bm25+vector for the seed even if those are also enabled as top-level
  retrievers. Graph is off by default (weight 0.0), so this double-run is only
  paid when an operator explicitly turns graph on; documented rather than
  optimized away.

### Tradeoffs
- Single-enabled-method "pass-through" vs. always running weighted RRF: chose
  pass-through for `lists.len() == 1` (the vector-only default) to avoid
  recomputing RRF scores for a list whose order RRF would preserve anyway.
- Oracle pipeline tests use a **bm25-only** config so they run without loading
  the real embedding model (oracle's `embed_query` rejects a mock
  `model_version`, so the vector path is not unit-testable in-crate without the
  ~100 MB model). The weighted multi-list fusion is covered by the vault-level
  unit tests instead; the oracle tests cover the plumbing (route on `mode:
  None`, single-method pass-through, no-methods-empty, explicit-mode back-compat).

### Open questions
- None blocking. The vector path and multi-method fusion inside `run_pipeline`
  are not exercised by an in-crate oracle test (model dependency); they are
  covered indirectly by the vault fusion tests + the `configured` eval target
  added in Phase 6, which runs against the real index.

## Phase 3: Exclude filters (stub + min-body-chars)

### Design decisions
- `SearchIndex::note_quality(path) -> Result<Option<String>>` is the new vault
  reader the stub filter needs. It uses the `quality` column (the architect-
  verified only stub signal in the `notes` table); the richer `[stub-body]`
  marker in `cortex-quality-issues` frontmatter is not a queryable column.
- `OracleMcpServer::apply_exclude_filters` runs as pipeline stage 5 on the fused
  candidates, in rank order. `stub` drops `quality` == `low` (case-insensitive);
  `min_body_chars` drops notes whose retrieved body is shorter. Both off => the
  list passes through. Per-candidate point lookups (≤ candidate_limit, ~50)
  rather than loading all low-quality notes, to avoid pulling full bodies for
  the whole low-quality set on every default query (stub is on by default).
- A candidate path that no longer resolves is left in place; the final
  `resolve_note_paths` skips it (consistent with existing missing-note handling).

### Deviations
- **Extracted `vault/src/search.rs`'s inline `#[cfg(test)] mod tests` into
  `vault/src/search/tests.rs`.** This was forced by the `otto bloat` gate
  (limit 3600): `search.rs` was already at 3597 lines, so `note_quality` + its
  test tipped it to 3633. The Rust conventions explicitly call inline `mod tests`
  blocks "drift [to] extract on sight," so the extraction is the sanctioned fix
  rather than a workaround - it drops `search.rs` to 1958 lines (permanent
  headroom) and moves zero production logic. Bundled into this phase because the
  bloat gate blocked the phase; flagged here so the large test-file move in the
  diff is not mistaken for new test content.

### Tradeoffs
- Per-candidate `note_quality` lookups vs. one `notes_by_quality("low", ...)`
  set-membership query: chose per-candidate point lookups. The single-query
  approach reuses existing code but loads full `NoteRow` bodies for the entire
  low-quality set (potentially hundreds) on every default query; ~50 indexed
  point lookups touch far less data on the old daemon host.
- Adding `quality` to `NoteRow` was rejected: `NoteRow::from_row` is shared by
  ~a dozen SELECTs, so adding a column would touch all of them (wide, error-
  prone) for a signal only the exclude filter needs.

### Open questions
- None.

## Phase 4: Cross-encoder rerank stage (off by default)

### Design decisions
- New `vault/src/search/rerank.rs` (+ `rerank/candle.rs`, `rerank/tests.rs`):
  - `Reranker` trait (port), `MockReranker` (lexical-overlap fake), and pure
    helpers `rerank_paths` + `project_batch_ms` are backend-independent and
    fully unit-tested - mirrors the `EmbeddingModel` / `MockEmbedder` split the
    advisor recommended so the reorder + budget-projection logic is testable on
    any host.
  - `CandleCrossEncoder` (gated `vec-candle`) is the production scorer:
    `BertModel` encoder + optional BERT pooler (dense+tanh) + a manual
    `candle_nn::Linear(hidden -> 1)` classification head, with prefix detection
    (`bert.` vs root) via `VarBuilder::contains_tensor`. Built from scratch
    because candle-transformers 0.10.2 exposes only `BertModel`, not a
    sequence-classification head (architect-verified).
  - Process-local lazy registry `get_or_load_reranker` mirrors the embedding
    registry; `prefetch_reranker` warms the hf-hub cache.
- `OracleMcpServer::maybe_rerank` is stage 4 of `run_pipeline`. Warmup probe
  times one pair, `project_batch_ms` over `available_parallelism`, and if the
  projection exceeds `latency_budget_ms` it WARNs and latches a process-global
  `RERANK_DISABLED` so subsequent queries skip the probe. Reranks only the head
  (`input_k`); the tail keeps fused order. Model-load failure and probe-scoring
  failure both fail open to fused order (never error the query).
- Candidate text = note `summary` (body fallback), truncated to
  `RERANK_TEXT_MAX_CHARS` (2000) before the tokenizer's own 512-token cap.

### Deviations
- None from the spec; Phase 4 is built as the doc's "from-scratch Candle
  pipeline," not a reuse of an existing rerank API (there is none).

### Tradeoffs
- Single-`Inner`-behind-`Mutex`, one padded forward over all pairs, vs. a
  rayon replica pool like the embedder. Chose the simpler single-model: the
  stage is off by default and probe-gated, so throughput tuning is premature;
  the probe's wave projection already accounts for parallelism conceptually.

### Open questions
- **The `CandleCrossEncoder`'s numerical correctness is unverified.** It
  compiles and follows the `BertForSequenceClassification` shape, but the daemon
  host is AVX-only (can't run fastembed; Candle scalar is ~7.5-15 s for 50
  pairs) and CI fetches no model, so it has not been run end-to-end. Risks not
  caught by compilation: (a) the `bert.`/`classifier` tensor-prefix assumption
  for `cross-encoder/ms-marco-MiniLM-L-6-v2`; (b) whether that model actually
  uses the pooler (the code detects + applies it if present); (c) score
  polarity (assumed higher = more relevant). These are validated only by
  enabling rerank against `sb oracle eval` on a capable host - per the
  advisor's acceptance bar (stage exists, wired, off by default, fail-open),
  not a measured lift. The probe makes a wrong/slow model degrade to fused
  order rather than hang or mis-rank silently on `desk`.
- Prefetch is exposed as the lib fn `vault::search::prefetch_reranker`; a `sb`
  CLI flag to call it is not added in this phase (the embedding prefetch lives
  under `sb cortex embed --prefetch-model`; a rerank equivalent can ride a
  later CLI pass). Documented in the Phase 6 template guidance instead.

## Phase 5: Query transforms (HyDE + multi-query, off by default)

### Design decisions
- New `oracle/src/transform.rs` (in oracle, NOT vault - it shells to
  `vault::fabric::run_pattern`, and `vault` stays LLM-free): pure
  `parse_transform_output` + `union_lists` (unit-tested) and `fabric_transform`
  (the untested LLM call).
- `run_pipeline` stage 1 computes a `Vec<String>` of query variants; stage 2
  retrieves every enabled method with each variant and `union_lists` before
  fusion. This is the identity for the common single-query case, so the existing
  pipeline tests still exercise the path; multi-query just supplies > 1 variant.
- Transform failure (or empty output) **fails open to the original query** - a
  flaky LLM degrades to normal retrieval, never an errored search.
- Resolved the doc's open question: multi-query union happens **before**
  per-retriever top-k truncation (each variant retrieves at full `top-k`, then
  the union feeds fusion), so each rewrite contributes at full depth.
- Resolved the doc's open question on the HyDE/transform model: it's the
  operator-configured `query-transform.pattern` + `query-transform.model` run
  through the on-box Fabric path (`TRANSFORM_BINARY = "fabric"`), keeping query
  text on-box by default (same trust boundary as the eval judge).

### Deviations
- **HyDE substitution is whole-pipeline, not vector-only.** Canonical HyDE
  embeds the hypothetical answer and does vector search; here the hypothetical
  answer replaces the query for *every* enabled retriever (so if bm25 is also
  enabled it matches the hypothetical's terms too). For the shipped default
  (vector-only) this is exactly canonical HyDE; the generalization only matters
  when an operator enables bm25/graph alongside HyDE. Chosen to avoid threading
  a separate per-retriever query through the pipeline; documented here.

### Tradeoffs
- A small free-function surface (`fabric_transform` + two pure helpers) instead
  of a `QueryTransformer` trait + mock like rerank's port. The transform's only
  testable logic is the output parsing and the union, both pure and covered;
  a trait/mock would add indirection without buying more coverage (the Fabric
  subprocess is the untestable part either way).

### Open questions
- The live HyDE/multi-query quality is unmeasured (no LLM in CI); validated by
  enabling the stage against `sb oracle eval`. Off by default, fail-open.

## Phase 6: Template, eval target, docs

### Design decisions
- `config/templates/oracle.yml.example` grows a fully-documented `retrieval:`
  block set to the shipped default (vector-only, stub-exclude), with rerank and
  query-transform present-but-disabled and a loud AVX/latency warning on rerank.
- `sb oracle eval` gains a `configured` target: `OracleMcpServer::
  run_configured_pipeline` (a thin wrapper that runs `run_pipeline` with
  `self.config.retrieval`, keeping the config field encapsulated). The eval's
  `retrieve` loop adds a `CONFIGURED_LABEL` ranked list per query, so the report
  scores the live pipeline alongside the 5 legacy modes + the fact ablation.
- Updated the root `CLAUDE.md` "Hybrid retrieval (Doc 2)" section and
  `oracle/AGENTS.md` to describe the configurable pipeline, the vector-first
  default flip, and where each stage lives.

### Deviations
- None.

### Tradeoffs
- The `configured` eval row is where the vector/rerank/transform paths actually
  get exercised end-to-end (against the real index + model), since they are not
  unit-testable in-crate (model/LLM deps). The eval is the acceptance gate the
  doc's Testing Strategy names.

### Open questions
- None.

## Phase 7: Embedding-input refinement + model upgrade (cortex-side)

### Design decisions
- **7a (title+summary prefix):** `cortex::embed::process_summary_batch` now
  embeds `format!("{title}\n\n{summary}")` (bare summary when title empty).
  `StaleTarget` gained a `title` field (both `stale_embedding_targets` SQL
  queries select `n.title`); new vault test accessor `embedding_text` lets the
  cortex test assert the stored text is `"T\n\nsummary"`. Existing rows re-embed
  on the next `sb cortex embed --backfill`.
- **7b (model A/B mechanism):** added a Candle BERT-family registry
  (`SUPPORTED_MODELS` in `vault/src/embedding/candle.rs`): bge-small (default) +
  bge-base (768-dim candidate). `CandleBertModel` carries its `dim`;
  `load_version(version, workers)` resolves `version -> (repo, dim)` and the
  forward path is dim-parameterized. embedding.rs gained `load_model_version`,
  `prefetch_model_version`, `is_supported_model_version`; `load_active_model` /
  `prefetch_active_model` are now thin default wrappers, and `get_or_load_model`
  (oracle's query path) loads any registered version. The default path stays
  byte-identical (CI green proves it). The A/B flip is `sb cortex embed --model
  bge-base-en-v1.5-candle` (no recompile): it re-pins `embedding_config` and the
  model-version change re-embeds; gate the switch on `sb oracle eval`.
- `load_daemon_model` now resolves the pinned version from `embedding_config`
  (not the compiled default) so a daemon restart picks up an A/B flip instead of
  re-pinning back to bge-small.

### Deviations
- **nomic-embed-text-v2 is NOT implemented as a loadable model** (architect-
  confirmed). It is a different architecture (`nomic_bert`, not `BertModel`), so
  a registry entry would be silently wrong - it needs its own loader. The doc's
  "bge-base **or** nomic" is a candidate menu; the BERT-family registry delivers
  the "implemented + cheap to A/B" mechanism with bge-base. Adding nomic later is
  a new loader, not a registry row, and is recorded here as the honest boundary.

### Tradeoffs
- A/B flip requires the cortex daemon stopped during the re-pin+backfill, then
  restarted (the running daemon would otherwise re-pin to its loaded model and
  fight the flip). Documented operational order; consistent with how any model
  change works. Not auto-coordinated (no daemon-reload-on-pin-change machinery -
  that would be gold-plating an off-by-default eval lever).
- bge-base inference *quality* is unmeasured here (no capable host / model in
  CI). Per the doc, that is the eval-gated operator step; the verifiable part
  (registry mapping, unknown-id error, default unchanged) is unit-tested.

### Open questions
- Does title+summary (7a) or bge-base (7b) actually beat the 0.876 vector
  baseline? Unmeasured in this environment; both are eval-gated operator
  decisions (`sb oracle eval` before/after), exactly as the doc specifies. The
  mechanisms ship; the defaults do not change blindly.
