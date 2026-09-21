# vault::search — Hybrid Retrieval Engine

> Local node. Parent crate: `../../AGENTS.md`. Consumers: oracle (query dispatch), cortex (index maintenance + embeddings).

## Purpose

SQLite-backed search for vault notes: BM25 via FTS5, brute-force cosine vector search, and reciprocal-rank fusion (RRF). Owns the on-disk index schema. Oracle queries it; cortex maintains it (the only embeddings writer).

## Entry Points

- `SearchIndex::open(db_path)` / `open_memory()`; `index_vault(vault_root)`.
- `search(query, tags, tags_all, note_type, status, limit) -> Vec<NoteRow>` (BM25). The same `tags: Option<&[String]>, tags_all: bool` pair (OR across the list by default, AND when `tags_all`) is on `list_notes`, `recent_notes`, `search_vector`, `tag_search`, `notes_by_creator`, `notes_by_source_domain`, `classify_stats`, and the three aggregate browsers `tag_stats` / `creator_stats` / `source_domain_stats`.
- `search_vector(query_vec, limit, tags, tags_all, note_type, status) -> Vec<VectorHit>` (cosine; feature `vec`).
- `stats() -> VaultStats { total_notes, by_tag (top 20), distinct_tags, by_type, by_status, schema_gaps }`; `tag_brief(tag, limit)`.
- `reciprocal_rank_fusion(bm25_paths, vec_paths, k=60, limit) -> Vec<FusedHit>`.
- `vector::{stale_embedding_targets, upsert_embedding, upsert_embeddings_batch}` (cortex re-embed loop).
- Graph/browse: `tag_search`, `cold_notes`, `orphan_notes`, `inbound_links`, `outbound_links`.
- Parsing: `parse_body_summary` (`## Summary`), `parse_body_claims` (`## Claims` bullets).

## Three Modes

1. **BM25 FTS5** — virtual table `notes_fts` over (title, tags, body, summary), kept in sync by triggers on `notes`; `AND`/`OR`/phrase/`-` syntax; zero embedding cost. Type/status/tags filters are SQL on the joined `notes` row, not FTS columns.
2. **Vector brute-force cosine** — scans every `note_embeddings` row, zero-copy dot-product from the BLOB; distance `1.0 - dot` for L2-normalized vectors; max-pool (min distance) per note; SQL filters (tags facet / type / status) applied before the dot loop.
3. **Hybrid RRF** — pull top `K_RRF_INPUT` (50) from each list, fuse via `reciprocal_rank_fusion(k=60, limit=20)`; per-note score `Σ 1/(60 + rank + 1)`; a note in only one list still scores.

## SQLite Schema Contracts

- **notes** — `path` (PK), schema columns, `body`, `summary`, `claims`, `modified_at`, `quality`, `classified`, `cortex-*` payload columns, `pinned`, `duplicate_group`, `inbound_link_count`, `search_hit_count`, `last_accessed_at`.
- **notes_fts** — FTS5 virtual table; INSERT/UPDATE/DELETE triggers on `notes` keep it synced (triggers are dropped+recreated on schema upgrade — FTS5 can't ALTER).
- **note_embeddings** (feature-gated) — `note_path`, `kind` (`summary`|`transcript-chunk`), `chunk_index`, `text`, `embedding` (BLOB f32 LE), `dim`, `model_version`, `produced_at`, `source_modified_at`; `UNIQUE(note_path, kind, chunk_index, model_version)`; FK CASCADE on `notes.path`.
- **embedding_config** — KV (`active_model`, `active_dim`); the canonical anti-drift store for cortex + oracle.

## Invariants

- **Staleness watermark:** `note_embeddings.source_modified_at < notes.modified_at` ⇒ stale. cortex writes the current `notes.modified_at` on upsert.
- **Model versioning:** `note_embeddings.model_version == embedding_config.active_model`; a backend swap changes the string and triggers re-embed via the staleness path.
- **Transcript eligibility** is generated from `NoteType::transcript_eligible()` — never hardcode the type list in SQL.
- **BLOB validation:** `validate_embedding_bytes(bytes, dim)` checks `len == dim*4` before the dot loop (no OOB reads).
- **Atomic embedding writes:** `upsert_embeddings_batch` is a single `BEGIN IMMEDIATE … COMMIT` so hybrid search never sees half-replaced vectors.
- **Atomic per-note indexing and removal:** `index_one` wraps the `notes` upsert and its `note_tags` maintenance in one SAVEPOINT; `remove_note` does the same for the `notes` + `note_tags` deletes and is the only removal path (`index_changed` and `remove_stale_notes` both call it). `index_changed` has no enclosing transaction, so both savepoints are load-bearing.
- **`note_tags` is the tags facet, `notes.tags` is the JSON for FTS.** Both are written by `index_one` from the same list, deduped once through `query::dedup_tags`, so `COUNT(*) FROM note_tags` equals the `json_each` pair count by construction (AC4). A tag filter goes through the facet (`push_tags_filter`, an indexed `EXISTS` subquery, input deduped before the AND-mode `= N` count) rather than `json_each`, which is a full scan plus parse. No FK on `note_tags` by choice: the connection does set `PRAGMA foreign_keys=ON` (`note_embeddings` needs the CASCADE), but the facet is maintained explicitly so it is rebuilt from the deduped list.
- **`VaultStats.distinct_tags` is the tag count.** `by_tag` is `top_tags(20)`; anything printing "N tags" reads `distinct_tags`.
- **Every indexed row's `tags` is valid JSON.** A note with no `tags` key stores `[]`, never `''`; the empty string is not valid JSON and made `json_each` error on 303 rows.
- **An empty tag list is not a filter.** `push_tags_filter` treats `Some(&[])` as no filter, so a caller threading an unset list never silently gets zero rows.

## Patterns

- **Dispatch hybrid (oracle):** `search(bm25)` + `embed_query()` → `search_vector(limit=50)` → `reciprocal_rank_fusion(k=60, limit=20)`.
- **Add an FTS5 column:** update the `CREATE VIRTUAL TABLE` in the schema-ensure path and the sync triggers (FTS5 cannot ALTER; the upgrade path drops and recreates).
- **Re-embed (cortex):** `stale_embedding_targets(Summary, active_model, batch)` → `load_active_model()` → embed in parallel → `upsert_embeddings_batch()`.

## Anti-patterns

- Don't decode/re-encode embeddings per query — dot-product directly from the borrowed BLOB.
- Don't skip the `source_modified_at` watermark — it's the re-embed trigger.
- Don't split `active_model`/`active_dim` across tables — keep them in `embedding_config`, updated in one txn.

## Constants

`RRF_K = 60` · `K_RRF_INPUT = 50` · `BGE_SMALL_EN_V15_DIM = 384` · `BUSY_TIMEOUT = 5s`.
