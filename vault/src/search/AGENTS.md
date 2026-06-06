# vault::search — Hybrid Retrieval Engine

> Local node. Parent crate: `../../AGENTS.md`. Consumers: oracle (query dispatch), cortex (index maintenance + embeddings).

## Purpose

SQLite-backed search for vault notes: BM25 via FTS5, brute-force cosine vector search, and reciprocal-rank fusion (RRF). Owns the on-disk index schema. Oracle queries it; cortex maintains it (the only embeddings writer).

## Entry Points

- `SearchIndex::open(db_path)` / `open_memory()`; `index_vault(vault_root)`.
- `search(query, domain, note_type, status, limit) -> Vec<NoteRow>` (BM25).
- `search_vector(query_vec, limit, …) -> Vec<VectorHit>` (cosine; feature `vec`).
- `reciprocal_rank_fusion(bm25_paths, vec_paths, k=60, limit) -> Vec<FusedHit>`.
- `vector::{stale_embedding_targets, upsert_embedding, upsert_embeddings_batch}` (cortex re-embed loop).
- Graph/browse: `tag_search`, `cold_notes`, `orphan_notes`, `inbound_links`, `outbound_links`.
- Parsing: `parse_body_summary` (`## Summary`), `parse_body_claims` (`## Claims` bullets).

## Three Modes

1. **BM25 FTS5** — virtual table over (title, domain, type, status, body, summary); `AND`/`OR`/phrase/`-` syntax; zero embedding cost.
2. **Vector brute-force cosine** — scans every `note_embeddings` row, zero-copy dot-product from the BLOB; distance `1.0 - dot` for L2-normalized vectors; max-pool (min distance) per note; SQL filters (domain/type/status) applied before the dot loop.
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

## Patterns

- **Dispatch hybrid (oracle):** `search(bm25)` + `embed_query()` → `search_vector(limit=50)` → `reciprocal_rank_fusion(k=60, limit=20)`.
- **Add an FTS5 column:** update the `CREATE TABLE` in the schema-ensure path and the sync triggers.
- **Re-embed (cortex):** `stale_embedding_targets(Summary, active_model, batch)` → `load_active_model()` → embed in parallel → `upsert_embeddings_batch()`.

## Anti-patterns

- Don't decode/re-encode embeddings per query — dot-product directly from the borrowed BLOB.
- Don't skip the `source_modified_at` watermark — it's the re-embed trigger.
- Don't split `active_model`/`active_dim` across tables — keep them in `embedding_config`, updated in one txn.

## Constants

`RRF_K = 60` · `K_RRF_INPUT = 50` · `BGE_SMALL_EN_V15_DIM = 384` · `BUSY_TIMEOUT = 5s`.
