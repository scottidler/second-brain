# vault::search — Hybrid Retrieval Engine

> Local node. Parent crate: `../../AGENTS.md`. Consumers: oracle (query dispatch), cortex (index maintenance + embeddings).

## Purpose

SQLite-backed search for vault notes: BM25 via FTS5, brute-force cosine vector search, and reciprocal-rank fusion (RRF). Owns the on-disk index schema. Oracle queries it; cortex maintains it (the only embeddings writer).

## Entry Points

- `SearchIndex::open(db_path)` / `open_memory()`; `index_vault(vault_root)`.
- `search(query, tags, tags_all, note_type, status, limit) -> Vec<NoteRow>` (BM25).
  `tags_all` picks the facet combinator: `false` = OR (any), `true` = AND (all).
  Every browse/stats query here takes the same pair (`Filter::and_tags`). All dynamic WHERE clauses are built by `search/filter.rs` `Filter` (bare `?` only, never numbered `?N`).
- `search_vector(query_vec, limit, …, &KindOffsets) -> Vec<VectorHit>` (cosine; feature `vec`).
- `reciprocal_rank_fusion(bm25_paths, vec_paths, k=60, limit) -> Vec<FusedHit>`.
- `vector::{stale_embedding_targets, upsert_embedding, upsert_embeddings_batch, swap_embeddings_batch, swap_kind_chunks, mark_nothing_to_embed_batch, drop_embedding_kind}` (cortex re-embed loop and its rollback verb); `EmbeddingKind::recipe()` and the per-kind `*_RECIPE` constants.
- Graph/browse: `tag_search`, `cold_notes`, `inbound_links`, `outbound_links`.
- Graph scoring: `expand_graph` (hop-limited BFS reaches; oracle scores them) and `personalized_pagerank(seeds, edge_kinds, min_weight, &PprParams) -> PprScores` (`graph/ppr.rs`, forward-push PPR, Andersen-Chung-Lang 2006). Both read edges through the one `neighbor_query`, so they traverse the same edges. PPR loads adjacency lazily into a request-local map through one prepared statement inside one read transaction (one snapshot while cortex rewrites edges, nothing cached across queries); the push cap returns the current estimate with a WARN. Its guarantee is `pi(v) - p(v) < epsilon * d(v)` (weighted degree), pinned against power iteration by `ppr_matches_power_iteration`.
- Topic map (`topic.rs`, vec-gated; design `docs/plan/2026-11-14-topic-map.md`): `replace_generation` (one `BEGIN IMMEDIATE` swap of `topic_state` + `topic_centroids` + `note_topics`, counter + 1), `assign_batch`, `unassigned(exclude_types, limit)` (in-scope rows with no topic row or a different `note_embeddings.vector_hash`), `in_scope_summaries(exclude_types)` (the training set), `remove_out_of_scope_topics(exclude_types)`, `topic_scope(exclude_types)` (eligible / in-scope / assigned counts, mean cosine, eligible notes with no summary row; one read transaction; eligibility is one SQL fragment, `eligible_filter`, shared by every scope reader), `clear_topics` (keeps the `generation` counter), `topic_state()` -> `TopicState::status(active_model, active_dim)` (`Absent | Valid | Stale`, corrupt state is `Err`), `topic_centroids()`. Readers (`topic/read.rs`, oracle's side): `overview(tags, tags_all)`, `in_tags(tag, limit)`, `notes(topic, generation, tags, tags_all, limit)`, `topic_snapshot()`, and the pure label rule `label_tags`; each runs in ONE read transaction that starts with the active-pin read, so state, counts, labels and exemplars come from one generation. Cortex is the only writer; no clustering here (`vault::kmeans`).
- Parsing: `parse_body_summary` (`## Summary`), `parse_body_claims` (`## Claims` bullets).
- Rerank (`rerank.rs`): the `Reranker` port, the `CROSS_ENCODERS` registry (`{ id, hf_repo, onnx_parity }`), and `get_or_load_reranker(model, &InferenceConfig, &CpuFeatures)`, which resolves the backend through `embedding::backend::resolve_with_parity` with the registry's parity bit and caches one cross-encoder per process. `CandleCrossEncoder` serves every entry; `OnnxCrossEncoder` only an entry whose `onnx_parity` is proven by `search::rerank::onnx::tests::onnx_ranks_like_candle_real_model` (`otto ignored`).

## Three Modes

1. **BM25 FTS5** - virtual table over (title, tags, type, status, body, summary); `AND`/`OR`/phrase/`-` syntax; zero embedding cost.
2. **Vector brute-force cosine** — scans every `note_embeddings` row, zero-copy dot-product from the BLOB; distance `1.0 - dot` for L2-normalized vectors; max-pool (min distance) per note, after adding each row's `KindOffsets` distance for its kind (`retrieval.methods.vector.kind-offsets`; all zero is the plain min); SQL filters (tags/type/status) applied before the dot loop.
3. **Hybrid RRF** — pull top `K_RRF_INPUT` (50) from each list, fuse via `reciprocal_rank_fusion(k=60, limit=20)`; per-note score `Σ 1/(60 + rank + 1)`; a note in only one list still scores.

## SQLite Schema Contracts

- **notes** — `path` (PK), schema columns, `body`, `summary`, `claims`, `modified_at`, `quality`, `classified`, `cortex-*` payload columns, `pinned`, `duplicate_group`, `inbound_link_count`, `search_hit_count`, `last_accessed_at`.
- **notes_fts** — FTS5 virtual table; INSERT/UPDATE/DELETE triggers on `notes` keep it synced (triggers are dropped+recreated on schema upgrade — FTS5 can't ALTER).
- **note_embeddings** (feature-gated) — `note_path`, `kind` (`summary`|`transcript-chunk`), `chunk_index`, `text`, `embedding` (BLOB f32 LE), `dim`, `model_version`, `produced_at`, `source_modified_at`, `recipe`, `vector_hash`; `UNIQUE(note_path, kind, chunk_index, model_version)`; FK CASCADE on `notes.path`. `vector_hash` is the embedding revision marker (`revision.rs`): lowercase hex SHA-256 of the exact blob, set by every writer through `search::vector_hash`; `migrate_vector_hash` in `ensure_schema` fills any NULL on open (the partial index `idx_note_embeddings_unhashed` keeps that check cheap), and a NULL a reader meets is an error. Neither `id` (reused: `INTEGER PRIMARY KEY` without `AUTOINCREMENT`, and the swap deletes before it inserts) nor `produced_at` (whole seconds) marks a re-embed. `embedding_examined` (the no-text sentinel) carries `recipe` too. `idx_note_embeddings_note_kind_model` is the stale query's join index: without it the planner joins through `(kind, model_version)` and one stale read on the live DB took ~20 s.
- **topic_state / topic_centroids / note_topics** (feature-gated) — KV generation metadata, `k` unit-norm f32 centroid BLOBs, one `note_topics` row per in-scope note (FK CASCADE on `notes.path`; `vector_hash` records the summary's `note_embeddings.vector_hash` it was assigned from). Every row belongs to `topic_state.generation`; there is no per-row generation. `topic_state.model_version` is renamed by `rename_model_id` with the other model-id columns.
- **edge_build_state** — per-note graph watermarks: `content_built_at` (the `notes.modified_at` content edges were built from) and `semantic_vector_hash` (the summary `vector_hash` semantic edges were built from, NULL when built unembedded). `semantic_edge_targets` selects a note whose active-model summary hash differs from it or is unrecorded; `record_edge_build` stores it from `note_summary_vector_hash`.
- **embedding_config** — KV (`active_model`, `active_dim`); the canonical anti-drift store for cortex + oracle.

## Invariants

- **Staleness watermark:** `note_embeddings.source_modified_at < notes.modified_at` ⇒ stale. cortex writes the current `notes.modified_at` on upsert.
- **Embed recipe:** every write path stamps the kind's `EmbeddingKind::recipe()`; a row or sentinel stamped lower is stale, so bumping one kind's `*_RECIPE` constant re-embeds that kind only, with no model-id change. Bump it whenever the text a kind embeds changes (assembly, chunker, window constants). Stale targets are DISTINCT per note before the `LIMIT`.
- **No text means no rows:** `mark_nothing_to_embed_batch` deletes the note's rows of that kind in the sentinel's transaction; `drop_embedding_kind` deletes the kind's sentinels with its rows, so a drop + backfill re-selects every note.
- **Model versioning:** `note_embeddings.model_version == embedding_config.active_model`; the id names the model, never the backend, so a backend swap changes nothing in the DB and triggers no re-embed. What keeps that safe is the `onnx_parity` gate on the `embedding::MODELS` registry: a model gets the bit only with a passing real-model parity test (`onnx_matches_candle_real_model` in `embedding/onnx/tests.rs`, `otto ignored`), and `resolve_backend` refuses ONNX for a model without it. A new model id (or a different vector space) is what triggers re-embed via the staleness path.
- **Transcript eligibility** is generated from `NoteType::transcript_eligible()` — never hardcode the type list in SQL.
- **BLOB validation:** `validate_embedding_bytes(bytes, dim)` checks `len == dim*4` before the dot loop (no OOB reads).
- **Atomic embedding writes:** `upsert_embeddings_batch`, `swap_embeddings_batch` (summary: per-note swap for the item's model) and `swap_kind_chunks` (chunk kinds) are each a single `BEGIN IMMEDIATE … COMMIT` so hybrid search never sees half-replaced vectors.
- **Atomic per-note indexing:** `index_one` wraps the `notes` upsert and its `note_tags` maintenance in one SAVEPOINT. `index_changed` has no enclosing transaction, so a failed facet write must roll the note row back with it or the two tables disagree.
- **`note_tags` is the tags facet, `notes.tags` is the JSON for FTS.** Both are written by `index_one` from the same list; a tag filter goes through the facet (`Filter::and_tags`, an indexed `EXISTS` subquery) rather than `json_each`, which is a full scan plus parse. The facet rows are deleted with the note in both stale-removal paths.
- **Every indexed row's `tags` is valid JSON.** A note with no `tags` key stores `[]`, never `''`; the empty string is not valid JSON and made `json_each` error on 303 rows.
- **An empty tag list is not a filter.** `Filter::and_tags` treats `Some(&[])` as no filter, so a caller threading an unset list never silently gets zero rows.

## Patterns

- **Dispatch hybrid (oracle):** `search(bm25)` + `embed_query()` → `search_vector(limit=50)` → `reciprocal_rank_fusion(k=60, limit=20)`.
- **Add an FTS5 column:** update the `CREATE TABLE` in the schema-ensure path and the sync triggers.
- **Re-embed (cortex):** `stale_embedding_targets(Summary, active_model, batch)` → `cortex::embed::load_embedder` (resolves `embed.backend`, loads via `vault::embedding::load_model_version`) → embed in parallel → `swap_embeddings_batch()`.

## Anti-patterns

- Don't decode/re-encode embeddings per query — dot-product directly from the borrowed BLOB.
- Don't skip the `source_modified_at` watermark — it's the re-embed trigger.
- Don't split `active_model`/`active_dim` across tables — keep them in `embedding_config`, updated in one txn.

## Constants

`RRF_K = 60` · `K_RRF_INPUT = 50` · `BGE_SMALL_EN_V15_DIM = 384` · `BUSY_TIMEOUT = 5s`.
