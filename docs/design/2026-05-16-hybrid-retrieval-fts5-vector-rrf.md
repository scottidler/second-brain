# Design Document: Hybrid Retrieval (FTS5 + Vector + RRF)

**Author:** Scott Idler
**Date:** 2026-05-16
**Status:** Implemented
**Review Passes Completed:** 5/5 + 3 architect rounds (Round 3 caught: search-blackout from index_vault deletion, inverted max-pool math, allocation foot-gun in BLOB decode, FK PRAGMA explicit doc)
**Parent:** [docs/scaling-roadmap.md](../scaling-roadmap.md) (Doc 2)

## Summary

Add local-only semantic retrieval on top of the existing FTS5 keyword index, fused via reciprocal rank fusion (RRF). Doc 1's distilled L2 summary becomes the embedding substrate; vectors are stored as a `BLOB` column on a regular SQLite table colocated with `notes`; semantic search is a pure-Rust brute-force cosine scan over the BLOB column; cortex owns the re-embed loop; oracle's `knowledge_search` gains a `mode: bm25 | vector | hybrid` parameter defaulting to `hybrid`. Ship in two implementation phases executed back-to-back: Phase A delivers summary-only hybrid retrieval end-to-end; Phase B adds chunked transcript embedding for Image, VoiceNote, Idea, Vocabulary, Video, and Thread. Phase B includes the borg/distillers amendment needed to render `## Transcript` for Video and Thread - the amendment is part of the work, not a prerequisite that blocks it.

**Storage choice rationale (deliberately boring):** the hybrid retrieval path runs BM25 and vector as two separate queries and fuses them with RRF in Rust. There is no single-query SQL composition of vector and FTS5; the design never benefits from a virtual table. So a regular `BLOB` column plus pure-Rust cosine in Rust code matches the actual usage perfectly, eliminates the `sqlite-vec` extension and its `unsafe` FFI transmute, removes the virtual-table FK-CASCADE trigger gymnastics, and at our scale (1,345 notes today, ~21K at the three-year horizon) is faster than walking an HNSW graph anyway. If we ever cross ~50-100K vectors, `hnsw_rs` (pure Rust, zero `unsafe` in caller code) can be added as a recoverable sidecar without changing the storage shape.

## Problem Statement

### Background

The current vault has ~7,000 notes growing at ~20/day. Retrieval today goes through `vault::search`'s FTS5 index over `(title, body, tags, summary, claims)`. FTS5 is BM25-based: it wins on proper nouns, exact terms, rare tokens, and the user's natural "I know I saved that word" queries. It loses on conceptual overlap - searching "agents that can use a browser" should surface notes titled "Computer Use", "Operator-style sandboxing", "playwright-mcp", even when none share lexical tokens with the query.

Doc 1 (Implemented, Phases 1-9) makes this problem tractable: every ingested source now produces a `Distilled { summary, claims, links, kind_specific, meta, transcript }` artifact rendered into the vault file as `## Summary` / `## Claims` / `## Links` / `## Transcript` body sections, parsed by `index_vault` into the SQLite index. The L2 summary is a dense, semantically clean 2-4 sentence prose representation of the source. That is exactly the substrate vector embeddings want.

### Problem

The existing system answers "find notes lexically similar to this query" well. It does not answer:

1. **Conceptual recall.** "What did I save about durable execution?" with zero shared tokens to the notes that actually cover Temporal, Restate, DBOS.
2. **Discovery.** "What else might be related to this note I'm reading?" - `find_similar` today uses FTS5-term overlap (`extract_search_terms` + OR-join), which is a noisy approximation of semantic similarity.
3. **Verbatim semantic recall on non-URL notes.** A 60-minute meeting voicenote distilled to a 4-sentence summary loses the verbatim phrasing the user remembers six months later. Phase 9 of Doc 1 preserves the verbatim text in a `## Transcript` body section for Image, VoiceNote, Idea, and Vocabulary kinds; that text needs to be reachable by semantic query, not just by keyword match.

### Goals

- Local-only semantic retrieval. No external API, no embeddings-as-a-service.
- Co-locate vector storage in the existing search SQLite database file so there is one index, one file lock, one backup.
- Default behavior is hybrid: `knowledge_search` without a `mode` parameter returns RRF-fused results.
- Pure-BM25 and pure-vector modes remain reachable for debug and A/B comparison.
- Sub-200ms p50 latency for a hybrid query on a 7K-note vault, including query-side embedding cost.
- Re-embed cadence and model-bump rollout are operator-controllable, not silent background work the user discovers via WAL bloat.
- The implementation lands in two phases (A then B) executed back-to-back as a single sustained push, organized for engineering coherence (Phase A is the retrieval substrate, Phase B is the transcript-chunked extension), not because Phase B is conditional on Phase A's outcome.

### Non-Goals

- Cross-encoder reranking. Out of scope for this doc.
- Multi-vault federation or cross-vault search.
- A separate "semantic_search" MCP tool. We extend the existing `knowledge_search` rather than fork the surface.
- Realtime push notifications for high-signal new notes.
- Reranking based on Doc 3 signals (search_hit_count, last_accessed_at). Doc 3 owns that scoring layer; Doc 2 produces a pure-similarity ranking that Doc 3 can later re-weight.
- A cross-dimension model swap as a routine operation. Switching from a 384-dim model to a 1024-dim model is a schema migration, not a hot swap. See "Model Bumps and Dimension Changes."
- Re-embedding on `canonical-tags.yml` change or on quality-field updates. Embeddings re-embed only when the *embedded text* changes (summary text or transcript text), not when adjacent metadata moves.

## Proposed Solution

### Overview

Three additions to the existing system:

1. **A new `vec` feature on the `vault` crate** that introduces a single `note_embeddings` regular table (BLOB column for the f32 vector) inside the same SQLite database file already used by `vault::search`. No virtual tables, no SQLite extension to load, no `unsafe` in our code. One DB file, one backup, one WAL.
2. **A new `vault::embedding` module** containing an `EmbeddingModel` port (trait), a `FastEmbedModel` adapter that loads `bge-small-en-v1.5` once and exposes batch and single-query embedding, and a pure-Rust `cosine_similarity` helper. Lives in `vault` so both the cortex re-embed loop and the oracle query path share one loader.
3. **A new `cortex embed` subcommand** (one-shot backfill) and a periodic re-embed job in the cortex daemon. Cortex is the single writer to `note_embeddings`; oracle reads only. This keeps `index_vault` focused on vault-derived data and respects the spirit of the one-way data flow rule (the embedding writer is a separate process operating on already-indexed SQLite content, not on the vault).

The hybrid query path in oracle:

```
knowledge_search(query, mode=hybrid, limit=10)
  │
  ├─► vault::embedding::embed_query(query)        # ~10-20ms CPU
  │      │
  │      └─► query_vec
  │
  ├─► vault::search::search_bm25(query, limit=50) # FTS5 MATCH
  │      │
  │      └─► top_50_bm25: Vec<(path, rank)>
  │
  ├─► vault::search::search_vector(query_vec, limit=50, kind_filter=summary)
  │      │
  │      └─► top_50_vec:  Vec<(path, rank)>       # brute-force cosine over BLOB rows
  │
  └─► reciprocal_rank_fusion(top_50_bm25, top_50_vec, k=60)
         │
         └─► top_10_hybrid
```

In Phase B, `search_vector` aggregates summary and transcript-chunk rows for the same note by max-pool before ranking, so a single note appears once in the result list.

### Architecture

```
┌────────────────────────────────────────────────────────────────┐
│ vault crate                                                    │
│                                                                │
│  vault::search        (existing - FTS5 over notes table)       │
│   + search_vector(query_vec, ...)        ← NEW (Phase A)       │
│                                                                │
│  vault::embedding     ← NEW MODULE                             │
│   - trait EmbeddingModel { embed_one, embed_batch, dim() }     │
│   - struct FastEmbedModel (bge-small-en-v1.5, lazy load)       │
│   - fn embed_query(&str) -> Vec<f32>                           │
│   - fn cosine_similarity(&[f32], &[f32]) -> f32                │
│   - fn chunk_transcript(...) (Phase B only)                    │
│                                                                │
│  feature flags:                                                │
│   - search (existing)                                          │
│   - vec    (new - adds fastembed dep, note_embeddings table)   │
└────────────────────────────────────────────────────────────────┘
                          │
        ┌─────────────────┼──────────────────┐
        ▼                 ▼                  ▼
┌──────────────┐  ┌──────────────────┐  ┌──────────────────────┐
│ oracle       │  │ cortex           │  │ borg                 │
│              │  │                  │  │                      │
│ reader of    │  │ writer of        │  │ unchanged            │
│ note_embed.  │  │ note_embeddings  │  │ (writes vault files; │
│              │  │                  │  │ does NOT embed)      │
│ - embed      │  │ - cortex embed   │  │                      │
│   query at   │  │   (backfill)     │  │                      │
│   request    │  │ - daemon embed   │  │                      │
│ - RRF fuse   │  │   job (periodic) │  │                      │
└──────────────┘  └──────────────────┘  └──────────────────────┘
```

### Data Model

#### Schema additions to the existing search DB

One new content table. Lives in the same SQLite file as `notes` / `notes_fts`. No virtual tables, no SQLite extensions, no triggers.

```sql
-- One row per (note, kind, chunk_index). Summary rows: chunk_index = 0.
-- Transcript chunks (Phase B): chunk_index = 0..N. Kinds without
-- transcripts have a single summary row only.
--
-- `embedding` is a BLOB of little-endian f32 values, `dim * 4` bytes
-- long. The `dim` column is stored explicitly so the decode step can
-- assert length matches without consulting embedding_config on every
-- row.
CREATE TABLE IF NOT EXISTS note_embeddings (
    id INTEGER PRIMARY KEY,
    note_path TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('summary', 'transcript-chunk')),
    chunk_index INTEGER NOT NULL DEFAULT 0,
    text TEXT NOT NULL,                  -- the source text that was embedded
    embedding BLOB NOT NULL,             -- little-endian f32, len = dim * 4
    dim INTEGER NOT NULL,                -- redundant w/ model_version, cheap
    model_version TEXT NOT NULL,         -- e.g. 'bge-small-en-v1.5'
    produced_at INTEGER NOT NULL,        -- unix seconds
    source_modified_at INTEGER NOT NULL, -- snapshot of notes.modified_at at embed time
    FOREIGN KEY (note_path) REFERENCES notes(path) ON DELETE CASCADE,
    UNIQUE (note_path, kind, chunk_index, model_version)
);

CREATE INDEX IF NOT EXISTS idx_note_embeddings_path
    ON note_embeddings(note_path);
CREATE INDEX IF NOT EXISTS idx_note_embeddings_stale
    ON note_embeddings(source_modified_at);
CREATE INDEX IF NOT EXISTS idx_note_embeddings_kind_model
    ON note_embeddings(kind, model_version);

-- Active model identifier. Single source of truth between oracle (query
-- embedder) and cortex (stored-embedding writer). Cortex updates this row
-- on `cortex embed --model X` invocation; oracle reads it at every
-- knowledge_search dispatch and uses the same model in embed_query.
CREATE TABLE IF NOT EXISTS embedding_config (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
INSERT OR IGNORE INTO embedding_config (key, value)
    VALUES ('active_model', 'bge-small-en-v1.5');
INSERT OR IGNORE INTO embedding_config (key, value)
    VALUES ('active_dim', '384');
```

**Why no virtual table:** the hybrid query path runs FTS5 and vector as two separate queries and fuses with RRF in Rust. There is no single-query SQL composition of vector and FTS5 that would benefit from `vec0` or any other virtual-table-backed KNN index. The boring shape (regular table, regular column, FK CASCADE, query in Rust) is the right shape for how the design actually uses the data.

**FK CASCADE works natively.** When a row is removed from `notes` (e.g. `remove_stale_notes` during reindex), every matching row in `note_embeddings` is deleted automatically by the foreign-key constraint. No triggers, no manual cleanup, no virtual-table gymnastics. This eliminates an entire category of Round-1 architect risk.

**Decoding the BLOB - zero-allocation dot product (load-bearing).** Do NOT materialize each stored vector into a `Vec<f32>` per query row. Allocating 21,000 `Vec<f32>` instances per query (~32 MB of allocator churn) would blow the latency budget. Instead, fold the byte iteration and the dot product into a single pass that never allocates:

```rust
// query_vec: &[f32] of length `dim` (the embedded query, kept in scope
//            for the whole scan)
// stored:    &[u8] of length `dim * 4` (one row's BLOB, borrowed from
//            the rusqlite Row, never copied)
fn dot_product_from_bytes(query_vec: &[f32], stored: &[u8]) -> f32 {
    debug_assert_eq!(stored.len(), query_vec.len() * 4);
    let mut dot = 0.0f32;
    for (i, c) in stored.chunks_exact(4).enumerate() {
        dot += query_vec[i] * f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
    }
    dot
}
```

Because bge-small outputs L2-normalized vectors, the query vector is also L2-normalized, and `cosine_similarity = dot_product`. No separate normalization step at query time. Distance is `1.0 - dot_product`.

No `unsafe`, no `bytemuck`, no `transmute`, no per-row allocation. The `dim` column on `note_embeddings` lets the decoder cheaply validate `stored.len() == dim * 4` before the loop - any mismatch is a clean validation error, not a panic.

**Scale envelope.** At 1,345 notes today and ~21K at the three-year projection, a brute-force cosine scan in pure Rust runs in single-digit milliseconds. If we ever cross ~50-100K vectors and the scan becomes user-visible, `hnsw_rs` (pure Rust, zero `unsafe` in caller code) drops in as a recoverable sidecar that consumes the same `embedding BLOB` rows - the storage shape does not change, and the API surface absorbs the upgrade behind `search_vector`.

#### Staleness contract (load-bearing)

Two distinct staleness paths, both driven by `notes.modified_at` bumps that `index_vault` records on every UPDATE. The implementation does *not* diff summary or transcript text - it relies on the cheaper `modified_at` proxy and accepts conservative re-embeds (see the calibration paragraph below). The rules as implemented:

1. **Summary row staleness.** When `index_vault` updates a row in `notes` and the new `notes.modified_at` is greater than any matching `note_embeddings.source_modified_at` for `kind = 'summary'`, the embedding is considered stale. Cortex's re-embed loop discovers the gap on its next scan (LEFT JOIN where `e.id IS NULL OR e.source_modified_at < n.modified_at`) and produces a new embedding. The matching row's deletion happens *implicitly* by upsert: cortex writes a new row with the current `source_modified_at`, replacing the stale one via the `UNIQUE (note_path, kind, chunk_index, model_version)` constraint.

2. **Transcript chunk staleness (Phase B).** Same `modified_at` rule, but with one extra step: because chunk boundaries shift when transcript text changes, cortex deletes *all* `kind = 'transcript-chunk'` rows for the note before re-chunking and re-inserting. There is no stable chunk-identity to preserve.

The proxy is intentionally lossy in the "no-diff" direction: any vault edit that bumps `modified_at` (including cosmetic changes to non-embedded sections) marks the embedding stale even when the summary/transcript text is unchanged. The cost calibration is in the paragraph below.

- `note_embeddings.source_modified_at < notes.modified_at` ⇒ stale.

This is conservative (any vault edit triggers re-embed regardless of whether the embedded text actually changed), but it is correct, cheap, and avoids the diff bookkeeping. The cost is occasional unnecessary re-embeds on cosmetic edits to non-embedded sections - including user-side Obsidian edits (tag additions, typo fixes in body sections that are not the summary or transcript). At ~20 ingests/day plus a realistic ceiling of ~50-100 user Obsidian edits/day, the worst-case is ~150 redundant re-embeds/day = ~7.5 CPU-seconds/day spread across the cortex daemon. WAL growth is bounded by the autocheckpoint. The cure (content-hashing the embedded text into the index and diffing against the stored hash) is heavier than the disease at this scale. Revisit if profiling shows daemon CPU or WAL size becoming user-visible.

Concretely, the re-embed loop scans:

```sql
-- Phase A: summary embeddings for every note.
SELECT n.path
FROM notes n
LEFT JOIN note_embeddings e
  ON e.note_path = n.path
 AND e.kind = 'summary'
 AND e.model_version = ?  -- current model
WHERE e.id IS NULL                       -- never embedded
   OR e.source_modified_at < n.modified_at;  -- stale
```

```sql
-- Phase B: transcript-chunk embeddings for transcript-eligible kinds only.
-- The note_type filter is load-bearing - without it, every Article and Repo
-- in the vault appears in this result set forever (they have no transcript
-- and never will, so e.id IS NULL is permanently true). The filter restricts
-- the scan to the kinds whose ## Transcript body section is actually
-- populated.
SELECT n.path
FROM notes n
LEFT JOIN note_embeddings e
  ON e.note_path = n.path
 AND e.kind = 'transcript-chunk'
 AND e.model_version = ?
WHERE n.note_type IN (
        'image', 'voice-note', 'idea', 'vocabulary',
        'video', 'thread'
      )
  AND (e.id IS NULL
       OR e.source_modified_at < n.modified_at);
```

#### What gets embedded per kind

| Kind        | Phase A           | Phase B                                              |
|-------------|-------------------|------------------------------------------------------|
| Article     | summary           | summary (short-form; URL re-fetch covers verbatim)   |
| Repo        | summary           | summary (no transcript exists; README is the source) |
| Thread      | summary           | summary + chunked `## Transcript`                    |
| Video       | summary           | summary + chunked `## Transcript`                    |
| Image       | summary           | summary + chunked `## Transcript`                    |
| VoiceNote   | summary           | summary + chunked `## Transcript`                    |
| Idea        | summary           | summary + chunked `## Transcript`                    |
| Vocabulary  | summary           | summary + chunked `## Transcript`                    |

**URL kinds split by content length, not by URL-vs-non-URL.** A 60-minute video distilled to a 4-sentence L2 summary cannot represent a single mention of "Temporal" at minute 45. The same logic applies to long X/Reddit threads. So:

- **Short-content URL kinds** (Article, Repo): summary preserves the salient content. Verbatim re-read is a single URL fetch.
- **Long-content URL kinds** (Video, Thread): summary is structurally lossy. Transcript chunking is part of Phase B.

**Borg/distillers amendment is part of Phase B2.** Borg's YouTube pipeline already fetches VTT subtitles (`borg/src/youtube.rs:21-485`); the thread pipeline already collects all post bodies. Neither currently renders a `## Transcript` body section for its published note (Phase 9 of Doc 1 restricted that section to non-URL kinds). Phase B2 amends `distillers::render` to render `## Transcript` for Video (VTT text, timestamps stripped) and Thread (concatenated post bodies). This is one render-time conditional plus a per-kind transcript source-of-truth lookup. The amendment ships in the same commits as Phase B2, not as a separate doc.

### API Design

#### `vault::embedding` module

```rust
pub trait EmbeddingModel: Send + Sync {
    fn dim(&self) -> usize;
    fn model_version(&self) -> &str;        // 'bge-small-en-v1.5'
    fn embed_one(&self, text: &str) -> Result<Vec<f32>>;
    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
}

pub struct FastEmbedModel { /* lazy-loaded model */ }

impl FastEmbedModel {
    pub fn load() -> Result<Self>;          // ~100MB load, ~1-2s cold
}

impl EmbeddingModel for FastEmbedModel { /* ... */ }

/// Convenience for the oracle query path. Uses a process-local
/// OnceLock keyed by model_version to avoid reloading the model per
/// query. Different model_version arguments load distinct models;
/// once loaded, both stay resident (acceptable: model bumps are rare).
pub fn embed_query(text: &str, model_version: &str) -> Result<Vec<f32>>;

// Phase B only:
pub fn chunk_transcript(
    text: &str,
    max_tokens: usize,    // 400
    overlap_tokens: usize, // 50
) -> Vec<String>;
```

The trait exists so cortex tests can inject a deterministic fake (`MockEmbedder` returning canned vectors) without loading the real 100MB model.

#### `vault::search` additions

```rust
impl SearchIndex {
    /// Brute-force cosine-similarity search over `note_embeddings` BLOB rows.
    /// Phase A: only `kind = 'summary'` rows. Phase B: aggregates summary +
    /// transcript-chunk rows per note via max-pool.
    ///
    /// Performance contract: at 21K vectors / 384 dims this runs in
    /// well under 20ms single-threaded; the regression benchmark in
    /// Phase A7 enforces the budget.
    pub fn search_vector(
        &self,
        query_vec: &[f32],
        limit: u32,
        filter: SearchFilter,
    ) -> Result<Vec<VectorHit>>;

    /// Insert (or replace) a (note_path, kind, chunk_index, model_version)
    /// embedding row. Called by cortex.
    pub fn upsert_embedding(
        &self,
        note_path: &str,
        kind: EmbeddingKind,
        chunk_index: u32,
        text: &str,
        embedding: &[f32],
        model_version: &str,
        source_modified_at: i64,
    ) -> Result<()>;

    /// Delete all embeddings for a note (used when the note is removed
    /// from the vault). Triggered automatically by FK CASCADE.
    pub fn delete_embeddings_for_note(&self, note_path: &str) -> Result<()>;

    /// List notes whose embeddings are missing or stale relative to
    /// notes.modified_at, for cortex's re-embed loop.
    ///
    /// For `kind = Summary`, every note is a candidate.
    /// For `kind = TranscriptChunk`, the impl applies a `note_type IN (...)`
    /// filter so only transcript-eligible kinds (Image, VoiceNote, Idea,
    /// Vocabulary, Video, Thread) appear in results. Without this filter,
    /// every Article and Repo would be returned forever; see the Staleness
    /// contract for the exact SQL.
    pub fn stale_embedding_targets(
        &self,
        kind: EmbeddingKind,
        model_version: &str,
        limit: u32,
    ) -> Result<Vec<StaleTarget>>;
}

pub struct VectorHit {
    pub note_path: String,
    pub distance: f32,    // cosine distance: 1.0 - cosine_similarity
}

pub enum EmbeddingKind { Summary, TranscriptChunk }
```

#### Oracle MCP surface

Extend `KnowledgeSearchRequest` with a `mode` field; do not add a new tool.

```rust
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SearchMode {
    Bm25,
    Vector,
    Hybrid,
}

pub struct KnowledgeSearchRequest {
    pub query: String,
    pub domain: Option<Domain>,
    pub note_type: Option<NoteType>,
    pub status: Option<Status>,
    pub detail: Option<DetailLevel>,
    pub limit: Option<u32>,

    /// Retrieval mode. Default: hybrid.
    pub mode: Option<SearchMode>,
}
```

The default-`hybrid` is set in the handler, not in serde, so omitting the field from MCP requests works without `#[serde(default)]` plumbing.

`oracle::server::knowledge_search` dispatches:

```rust
// Read the active model from the DB so oracle and cortex never disagree.
let active_model = db.active_embedding_model()?;  // 'bge-small-en-v1.5'

match req.mode.unwrap_or(SearchMode::Hybrid) {
    SearchMode::Bm25   => db.search(query, filters, limit),
    SearchMode::Vector => {
        let q_vec = vault::embedding::embed_query(query, &active_model)?;
        db.search_vector(&q_vec, limit, filters)
    }
    SearchMode::Hybrid => {
        let q_vec = vault::embedding::embed_query(query, &active_model)?;
        let bm25 = db.search(query, filters, K_RRF_INPUT)?;
        let vec  = db.search_vector(&q_vec, K_RRF_INPUT, filters)?;
        reciprocal_rank_fusion(&bm25, &vec, RRF_K, limit)
    }
}
```

#### Cortex subcommand

```
cortex embed                  # one-shot pass: embed everything missing/stale
cortex embed --backfill       # alias for above; explicit for the first run
cortex embed --kind summary   # restrict to a single embedding kind
cortex embed --model bge-small-en-v1.5  # explicit model selection
cortex embed --batch-size 64  # tune memory vs throughput
cortex embed --prefetch-model # download model weights then exit; no embeddings written. Use this on install machines that have network during install but may be offline at oracle first-query time
```

The cortex daemon runs `cortex embed` on a configurable cadence (default: every 10 minutes). Most calls find zero stale rows and return immediately.

#### RRF constants

```rust
const RRF_K: usize = 60;            // standard k from the literature
const K_RRF_INPUT: u32 = 50;        // top-K pulled from each list before fusion
```

RRF score per note:

```
score(d) = Σ_list 1 / (k + rank_list(d))
```

Notes appearing in only one list still get a contribution (the other list's `rank_list(d)` is treated as infinity, contributing 0). Sort by descending score; return top `limit`.

### Implementation Plan

The plan ships in two phases executed back-to-back. Phase A delivers the hybrid retrieval path end-to-end with summary-only embeddings. Phase B adds transcript chunking for Image, VoiceNote, Idea, Vocabulary, Video, and Thread. Both phases are committed work, not contingencies.

#### Phase A1: vault `vec` feature flag + schema scaffolding
**Model:** sonnet
- `cargo add fastembed --optional` in `vault/Cargo.toml`, gated behind the new `vec` feature
- No `sqlite-vec`, no other extension - the storage shape is a regular table with a `BLOB` column
- Add the `note_embeddings` table (BLOB column for the f32 vector, `dim` column for length validation, `model_version`, `source_modified_at`) and the `embedding_config` key/value table to `ensure_schema`, gated by the feature. No virtual tables, no triggers
- Set `PRAGMA busy_timeout = 5000` at connection open in `SearchIndex::open` so oracle and cortex serialize cleanly when their writers overlap
- Wire `vec` into oracle's and cortex's feature flags on `vault`
- Migration test: existing DB with `notes` only opens cleanly under the new schema; new tables are created idempotently
- Migration test: deleting a row from `notes` cascades cleanly to `note_embeddings` via the native FK CASCADE (no trigger needed, no virtual table to worry about)
- Migration test: a malformed BLOB (length not divisible by 4, or length mismatched with `dim`) is rejected at decode time with a clean error, not a panic

#### Phase A2: `vault::embedding` module - port + FastEmbed adapter
**Model:** opus
- Define the `EmbeddingModel` trait
- Implement `FastEmbedModel` with lazy single-instance load (`std::sync::OnceLock<Arc<TextEmbedding>>`)
- Implement `embed_query(text: &str, model_version: &str) -> Result<Vec<f32>>` using the OnceLock
- Add a `MockEmbedder` test helper returning deterministic vectors (e.g. hash-derived) so downstream code can be unit-tested without the real model
- Verify dimension (384) and document the constant
- **Load timing in oracle is lazy, not eager.** The OnceLock is populated on the first `mode != bm25` call, not at server start. Rationale: oracle's MCP startup is on a latency-sensitive path (Claude Desktop handshake) and pure-BM25 callers should not pay the ~1-2s model-load cost they never use. The first `mode=hybrid` or `mode=vector` call after process start incurs the load latency; subsequent calls hit the OnceLock at ~0ms overhead. Cortex, by contrast, loads eagerly at the start of any `cortex embed` invocation because every cortex embed call uses the model
- **Cortex daemon model lifecycle:** cortex daemon keeps the model resident between embed ticks (no unload between cadence intervals). ~100MB resident in cortex is acceptable; alternative (load-and-drop per tick) burns 1-2s of CPU every 10 minutes for no gain

#### Phase A3: `search_vector` + RRF fusion
**Model:** opus
- Implement `SearchIndex::search_vector` (Phase A reads only `kind = 'summary'` rows)
- Implement `upsert_embedding`, `delete_embeddings_for_note`, `stale_embedding_targets`
- Implement `reciprocal_rank_fusion(bm25, vec, k, limit)` as a pure function in `vault::search`
- Unit tests with `MockEmbedder` and golden-vector fixtures asserting fusion math

#### Phase A4: index\_vault staleness wiring
**Model:** opus
- **`index_vault` does NOT delete embeddings on UPDATE.** This is a deliberate design contract: deleting on reindex would create a search-blackout window (the note disappears from hybrid search for up to one cortex cadence interval = 10 minutes) every time it is reindexed. A slightly-stale embedding still serves the query meaningfully; a missing one does not. The staleness signal is just `source_modified_at < notes.modified_at`, which cortex's re-embed loop discovers on its next tick. When cortex writes the new embedding, the `UNIQUE (note_path, kind, chunk_index, model_version)` constraint causes an upsert that atomically replaces the stale row - no blackout, no orphans.
- **Verify `PRAGMA foreign_keys = ON` is set in `SearchIndex::open`.** SQLite disables FK enforcement by default for backwards compatibility. Without this PRAGMA, `ON DELETE CASCADE` silently fails and `note_embeddings` slowly fills with orphaned rows after every `remove_stale_notes` pass. The PRAGMA must run BEFORE the first INSERT into `notes` to be effective. This is already in place at `vault/src/search.rs:186` for FK enforcement in general; the test below confirms it is still enforced after the new schema lands.
- New-row INSERT branch needs no embedding handling - cortex's stale-target scan picks up the missing rows
- Note deletion (in `remove_stale_notes`) triggers the native FK CASCADE; verify
- Tests:
  - Index a note, embed it via a stub, modify the note, re-index. Assert the OLD embedding row is still present (no blackout); assert cortex's stale-target scan flags it. After cortex's upsert runs, assert there is exactly one row (the new one replaced the old via UNIQUE constraint)
  - Delete a note from the vault, re-run `index_vault`. Assert all matching `note_embeddings` rows are gone (FK CASCADE works)
  - Disable the PRAGMA temporarily in a test fixture, repeat the deletion test, assert orphans remain. This guards against a future "performance optimization" disabling FK enforcement

#### Phase A5: `cortex embed` subcommand + daemon job
**Model:** opus
- Add `cortex embed` subcommand to the cortex CLI
- **Transaction discipline is load-bearing.** The SQLite write lock must NOT be held across the CPU inference step. With batch=64 and ~50ms/note, holding a transaction across `embed_batch` would lock the DB for ~3.2 seconds per batch and starve oracle's `index_vault` writes (which exceed `busy_timeout = 5000` under load). The loop body must be:
  1. **Read phase (auto-commit, no transaction):** query `stale_embedding_targets` to pull the next 64 (path, kind, source_modified_at, text) rows. Connection returns to idle immediately
  2. **Inference phase (no SQLite interaction):** call `embed_batch(&texts)`. ~3 seconds of CPU. No DB lock held. Oracle's `index_vault` writes proceed normally during this window
  3. **Write phase (one short transaction):** `BEGIN IMMEDIATE`, call `upsert_embedding` for each result row, `COMMIT`. This transaction stays under ~50ms because there is no CPU work inside it. Oracle may briefly wait on its next `index_vault` UPDATE; the wait is bounded by the write transaction length, not by inference time
- This is the single most important implementation invariant in Phase A. A reviewer reading the cortex embed loop should be able to point at the `BEGIN IMMEDIATE` line and confirm `embed_batch` is *not* called between it and the matching `COMMIT`. Add an explicit code comment at the transaction boundary stating this contract
- Loop body: open the search DB once via `vault::search`, load `FastEmbedModel` once, then run the read-inference-write loop until `stale_embedding_targets` returns empty
- Edge cases:
  - Skip notes with empty or whitespace-only `summary` - these are failed-distillation rows and embedding them produces garbage vectors. Log at `WARN` so the user sees the gap
  - Skip notes whose `summary` exceeds the model's 512-token limit (rare; truncation is acceptable but log at `INFO`)
  - Crash recovery is implicit: `source_modified_at < notes.modified_at` query re-discovers the partial batch on the next pass
- Add a daemon job calling the same code path on the configured cadence
- Daemon job acquires a file lock (`fs2::FileExt`) to prevent concurrent `cortex embed` invocations from racing the daemon
- Config: `embed.cadence-minutes` (default 10), `embed.batch-size` (default 64). The active model is read from `embedding_config.active_model` in the DB, not the cortex config file. `cortex embed --model X` updates the DB row.

#### Phase A6: oracle `mode` parameter + hybrid dispatch
**Model:** opus
- Add `SearchMode` enum to `oracle::tools`
- Add `mode: Option<SearchMode>` to `KnowledgeSearchRequest`
- In `oracle::server::knowledge_search`, dispatch to `search`, `search_vector`, or hybrid as designed
- Update the `knowledge_search` tool description to mention modes
- Tool-schema test: ensure `SearchMode` is reflected correctly in the JsonSchema
- Edge cases:
  - Empty/whitespace query string: return `Err("query is empty")` before invoking the embedder
  - `mode=vector` or `mode=hybrid` on a vault with zero embedded notes (fresh install pre-backfill): return cleanly with an empty result list; log at `WARN` once per process so the user knows backfill hasn't run
  - `K_RRF_INPUT` (50) is the number of candidates pulled from each list before fusion; the user's `limit` (default 10) is applied after fusion. This is explicit in the dispatch - over-pulling 50 is cheap and improves recall

#### Phase A7: regression test fixture + latency benchmark
**Model:** sonnet
- 20-query regression fixture: real vault note hashes (or stable subset) with expected top-3 hit sets for each of `bm25`, `vector`, `hybrid`
- Asserts run on every CI build: hybrid must recover at least the union's top-3 for 18/20 queries (tolerance for ranker noise)
- `cargo bench` target: end-to-end `knowledge_search` latency at 7K and 21K synthetic vaults. Budget: hybrid p50 ≤ 200ms including query embedding (~10-20ms)
- Document the benchmark methodology in the doc itself; reproducible from CI

#### Phase A8: rollout + docs
**Model:** sonnet
- Add `oracle install` / `cortex install` paths to the deploy targets (model is downloaded on first `FastEmbedModel::load` - see Open Questions on whether to pre-fetch in build.rs)
- Update `scaling-roadmap.md` to mark Doc 2 Phase A Implemented
- Update top-level `CLAUDE.md` with the new `cortex embed` subcommand and the `mode` parameter
- Update oracle's MCP description if its top-line surface text mentions retrieval (it does - see oracle/src/server.rs `get_info()`)

#### Phase B1: chunker
**Model:** opus
- Implement `chunk_transcript(text, max_tokens, overlap_tokens) -> Vec<String>`
- Tokenization via fastembed's tokenizer (so chunks fit the model's 512-token window with margin)
- Sliding window with 50-token overlap so claims that straddle chunk boundaries appear in both
- Tests: short input (one chunk), long input (multiple chunks, overlap verified), pathological inputs (single very long sentence, all whitespace)

#### Phase B2: transcript embedding path in cortex + distillers amendment
**Model:** opus
- **Amend `distillers::render` to render `## Transcript` for kind=Video and kind=Thread.** This is part of Phase B2 work, not a separate document. Video transcript source is the VTT text fetched by `borg/src/youtube.rs:21-485` with timestamps stripped. Thread transcript source is the concatenated post bodies in chronological order, one post per paragraph. The render-time conditional in `distillers::render` flips from "non-URL kinds only" to "any kind with a transcript artifact."
- Extend the cortex re-embed loop to read `## Transcript` from notes whose kind is one of: Image, VoiceNote, Idea, Vocabulary, Video, Thread
- **The Phase B stale-target query MUST filter by `notes.note_type`.** Without it, every Article and Repo in the vault matches `e.id IS NULL` permanently (they have no transcript and never will), causing the cortex daemon to pull thousands of ineligible note paths every tick and write zero rows. The filter restricts the scan to the kinds whose `## Transcript` body section is actually populated. The exact SQL is in the "Staleness contract" section
- Add a regression test: in a synthetic vault with 100 Articles and 1 VoiceNote, after one full embed pass, the next `stale_embedding_targets(kind=TranscriptChunk)` call returns zero paths. A regression in the `note_type` filter (e.g. someone removes it for "performance") immediately fails this test
- Chunk, embed batch, then perform an **atomic chunk swap** for the note inside the Phase A5 write transaction: `BEGIN IMMEDIATE; DELETE FROM note_embeddings WHERE note_path = ? AND kind = 'transcript-chunk'; INSERT ... (one row per new chunk); COMMIT;`. The DELETE is necessary because re-chunking shifts boundaries - there is no stable chunk-identity to preserve across edits, so old chunks must be removed before new ones are written. Keeping the delete-and-insert in one transaction means hybrid search never sees a half-replaced chunk set
- Important: `index_vault` does NOT delete transcript chunks on update either (same blackout-avoidance contract as Phase A4 summary rows). Cortex's re-embed loop owns the chunk lifecycle in its entirety. The staleness signal is `source_modified_at < notes.modified_at`, same as summary rows
- Backfill: after the amendment lands, existing Video and Thread notes in the vault do not yet have `## Transcript` body sections. Run `cortex summarize --backfill --kind=video,thread` to re-render those notes with the new transcript section before the embed pass picks them up. This reuses Doc 1's existing backfill machinery.

#### Phase B3: max-pool aggregation in `search_vector`
**Model:** opus
- For each candidate note, compute its score as `min(summary_distance, chunk_distances...)` - the single best-matching representation wins. In cosine-distance space, smaller is closer, so the minimum distance across {summary row, every transcript-chunk row} is the "max-pool similarity" of the note. The earlier draft of this doc had it inverted (taking the *worst* representation's distance); that bug would have catastrophically penalized notes whose value lives in a single transcript chunk, e.g. the architect's hardest-question example of a 60-minute video with one mention of "Temporal" at minute 45
- Implementation note: a single SQL query returns all rows for the candidate notes (summary + chunks); the aggregation runs in Rust. With the zero-allocation dot product per row, even 21K candidates * average 1.2 rows per note = ~25K rows aggregated in single-digit milliseconds
- Return one row per note
- Regression test: a transcript-only match (the query token only appears in a chunk, not in the summary) is reachable via vector search for non-URL kinds. The test fixture has one note where the summary is intentionally orthogonal to the chunk's content; the query that matches the chunk must surface this note in top-3

#### Phase B4: regression fixture extension + rollout
**Model:** sonnet
- Add 5-10 non-URL queries to the regression fixture whose expected hits depend on transcript-chunk recall
- Verify Phase A queries remain green
- Update `scaling-roadmap.md` to mark Doc 2 Phase B Implemented

## Alternatives Considered

### Alternative 1: Skip vector entirely; rely on better FTS5

- **Description:** Tune FTS5 ranking with custom weights, add stemming, add synonym expansion. No embeddings.
- **Pros:** Zero new dependencies. No model download. No CPU cost per query.
- **Cons:** Caps out at lexical retrieval. The "agents that can use a browser" → "Computer Use" leap is unreachable without semantic similarity. Synonym dictionaries are a maintenance burden that grows with the vault.
- **Why not chosen:** The roadmap's stated discovery problem (#2: "what did I save about X that I forgot existed") is exactly the case where lexical retrieval falls short. Defer the work and Doc 2's whole premise evaporates.

### Alternative 2: External embeddings API (OpenAI, Cohere)

- **Description:** Call a remote embeddings API at ingest and at query time.
- **Pros:** No local CPU cost, no model download, higher-quality models available.
- **Cons:** Network dependency for every query (latency, reliability). Cost per query. Vault data leaves the machine. Violates the "local-only" property of the rest of the system (Fabric is local via Ollama, distillers are local, etc.).
- **Why not chosen:** Local-only is a deliberate constraint of the second-brain system; adding a remote dependency for retrieval contradicts that.

### Alternative 3: Embed full note body instead of L2 summary

- **Description:** Embed `notes.body` directly, ignoring the distilled summary.
- **Pros:** Captures more verbatim content. No dependency on Doc 1's distillation quality.
- **Cons:** Body is large and noisy; embeddings of long heterogeneous text generalize poorly. Re-embedding the whole body on every edit (even cosmetic) is expensive. Phase 9's `## Transcript` already handles the verbatim case for non-URL kinds without forcing it on URL kinds where it adds nothing (the source URL is the archive).
- **Why not chosen:** L2 summary is the right substrate by design. Doc 1's whole point was producing a dense semantic representation; using the body bypasses that work.

### Alternative 4: Vector-only retrieval (no fusion)

- **Description:** Drop FTS5 from the query path; vector similarity is the only signal.
- **Pros:** Simpler. One query path.
- **Cons:** BM25 wins on proper nouns, exact phrases, rare tokens, and acronyms - exactly the cases where the user "knows the word." Vector embeddings systematically lose on these. RRF is one screen of code and recovers both behaviors.
- **Why not chosen:** Hybrid is the established best practice and the cost (RRF function + one extra query) is negligible.

### Alternative 5: Dedicated vector DB file (LanceDB, qdrant)

- **Description:** Vectors live in a separate file/process from FTS5.
- **Pros:** Optimized ANN indexes (HNSW), parallel scaling, mature tooling.
- **Cons:** Two indexes to keep in sync. Two backup paths. Two file locks. A second process to run on the user's machine. Pure-Rust brute-force cosine at 7K-21K vectors runs in ~5-20ms - well within budget; HNSW only matters at 100K+.
- **Why not chosen:** Co-located in the existing search DB is the simpler model and the scale doesn't justify a separate store. Revisit at 100K+ vectors.

### Alternative 6: Embed in oracle directly (skip cortex)

- **Description:** Oracle's reindex path embeds new notes inline as part of reindex.
- **Pros:** No second writer to the DB; one process owns the index.
- **Cons:** Embeddings are CPU-bound (~50ms per note). Coupling re-embed to the reindex path means a vault-wide reindex blocks on embedding ~21K notes (~17 minutes of CPU). Reindex must stay cheap. Cortex's daemon scheduler is the right place for periodic CPU work.
- **Why not chosen:** Decouples cadence from reindex; cortex already runs as a daemon with rayon and a cron-like scheduler.

## Technical Considerations

### Dependencies

New:
- `fastembed` - local embedding inference. Bundles ONNX runtime; first load downloads the model from HuggingFace.

That is the entire dependency footprint. No `sqlite-vec`, no SQLite extension to load, no FFI bindings beyond what rusqlite already brings. Vector storage is a regular `BLOB` column on a regular table; vector similarity is a pure-Rust cosine function. The `vec` feature gates only `fastembed`, the `note_embeddings` schema, and the new `vault::embedding` module.

### Performance

- **Embedding query (CPU, bge-small):** ~10-20ms per query string
- **FTS5 BM25 over 7K notes:** <5ms
- **Pure-Rust brute-force cosine over 1,345 384-dim vectors (today):** <2ms
- **Pure-Rust brute-force cosine over 7K 384-dim vectors:** ~5-10ms (~10MB scan)
- **Pure-Rust brute-force cosine over 21K 384-dim vectors (3-year):** ~15-25ms (~32MB scan)
- **RRF fusion:** <1ms (in-memory sort)
- **Total p50 hybrid query budget:** ≤ 200ms (includes detail-level formatting and JSON serialization)

At 21K notes (3-year projection), KNN scan rises proportionally to ~30-60ms. Still under budget. Beyond 100K, swap to an ANN-indexed store; that is out of scope for Doc 2.

Re-embed cost: ~50ms per note on CPU. Backfilling 7K notes is ~6 minutes wall-clock single-threaded; rayon-parallel cuts that to ~1-2 minutes on a typical multi-core laptop. Daily steady-state re-embeds (~20 ingests/day) finish in ~1 second.

### Model Bumps and Dimension Changes

The `model_version` column handles **same-dimension** model swaps (e.g. `bge-small-en-v1.5` → `bge-small-en-v1.6`):

1. Run `cortex embed --model bge-small-en-v1.6 --backfill`. This updates the `embedding_config.active_model` row in SQLite and writes new rows with `model_version = 'v1.6'` alongside existing `v1.5` rows.
2. While the backfill runs, oracle sees `active_model = 'v1.6'` and immediately starts embedding queries with the new model. Hybrid queries against still-old-model notes degrade to BM25 for those notes during the rollover window (the new-model rows aren't present yet for them).
3. Backfill completes; the rollover window closes.
4. `cortex embed --gc-old-models` (or manual `DELETE FROM note_embeddings WHERE model_version != 'v1.6'`) reclaims space.

**Cross-dimension model swaps** (e.g. 384 → 1024) are dramatically simpler than they would be with a virtual table:

1. The `dim` column on `note_embeddings` carries the dimension per row. Different model_versions can have different `dim` values living in the same table.
2. The `embedding_config.active_dim` row tells oracle which dimension to expect at query time, so it can filter to matching rows: `SELECT ... WHERE model_version = ? AND dim = ?`.
3. Migration: run `cortex embed --model bge-m3-1024 --backfill`. Old `dim=384` rows stay until garbage-collected. Oracle queries against `dim=1024` rows once backfill completes.
4. No schema migration. No new table. No virtual-table dim lock. This is the single biggest practical win of the BLOB-column shape over the virtual-table shape.

### Security

Local-only. No network calls at query or embed time (after first-load model download). No vault data leaves the machine.

The model download on first load happens over HTTPS from HuggingFace; verify checksum if the crate supports it. Document in `cortex embed --help` that first run downloads ~100MB.

### Testing Strategy

- **Unit:** RRF function with synthetic rank lists; chunker boundary cases; staleness query SQL; `MockEmbedder` deterministic-vector tests
- **Integration:** real fastembed model loaded once across the test suite via `OnceCell`; embed a tiny synthetic vault; verify hybrid recovers expected hits
- **Regression fixture:** 20-30 real queries with expected top-3 hits - run on every PR touching retrieval, with tolerance for ranker noise (18/20 must match expected union)
- **Benchmark:** `cargo bench` over 7K and 21K synthetic vaults; assert p50 ≤ 200ms

The benchmark and regression fixture both live under `vault/benches/` and `vault/tests/regression/` so they survive crate moves.

### Rollout Plan

1. **Phase A1-A8 ships behind the `vec` feature, opt-in at first.** Oracle and cortex are built with `vec` enabled in the deploy targets; the feature gate exists for crates that don't want the embedding dependency.
2. **First run after install:** preferred path is `cortex embed --prefetch-model` immediately after install, while network is known to be available. This downloads the ~100MB ONNX weights to the fastembed cache (OS-standard) and exits. Subsequent oracle queries and cortex embed runs hit the local cache and need no network. Document the ~100MB one-time download in the install path. Fallback: if `--prefetch-model` is skipped, the cortex daemon downloads on its next embed tick (or on a manual `cortex embed --backfill` invocation), assuming network is available then.
3. **Initial backfill:** user runs `cortex embed --backfill` once, ~1-2 minutes on the current ~7K vault.
4. **Steady state:** daemon re-embeds new and modified notes every 10 minutes.
5. **Query default switches to hybrid once Phase A6 ships.** Pre-A6 queries continue to be BM25-only. The default flip is the user-visible cutover; document it in the release notes.
6. **Phase B ships immediately after Phase A.** Both phases land back-to-back with no soak period between them. The user uses the system every day; observing the effect of Phase B will happen through normal use, not a deliberate evaluation window.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| fastembed first-load downloads from HuggingFace; offline systems fail | Med | High | Document the requirement; consider a `fastembed-prefetch` build-time step that bundles the model. Cache path is OS-standard; subsequent runs are offline. |
| WAL bloat during backfill | Med | Med | Bound batch size to 64; `PRAGMA wal_autocheckpoint = 1000` (default). If a daemon-driven backfill grows WAL beyond 100MB, add an explicit `PRAGMA wal_checkpoint(TRUNCATE)` between batches |
| Re-embed loop starves oracle queries / index_vault writes via SQLite lock contention | Low | High | **Structural fix**: Phase A5's transaction discipline mandates that the SQLite write lock is acquired only around the upsert (post-inference). `embed_batch` runs in auto-commit context with no lock held. Write transactions stay under ~50ms regardless of batch size. **Belt-and-suspenders**: `PRAGMA busy_timeout = 5000` at connection open in both crates handles the residual overlap window. If Phase A5 regresses (someone wraps the inference call inside `BEGIN`), the A5 invariant test catches it: a `cortex embed` batch with N=64 must produce a write transaction wall-clock under 200ms in CI |
| Distillers amendment for Video/Thread `## Transcript` rendering breaks existing notes | Low | Med | Amendment is a kind-conditional in `distillers::render`; existing renders for non-URL kinds are unchanged. Backfill for existing Video and Thread notes runs via `cortex summarize --backfill --kind=video,thread` once the amendment lands. Snapshot test on a fixture Video note guards the render output |
| Oracle's query embedder and cortex's stored embeddings drift onto different models | Low | High | Active model and dimension live in the `embedding_config` SQLite table - single source of truth read by both processes. Mismatch is impossible at the DB level. The per-process config files only override the *target* model for the next `cortex embed --model X` run, which writes the new value back to `embedding_config` |
| Embedding quality on technical content is poor with bge-small | Med | Med | Regression fixture catches it. If the 18/20 threshold fails, bump to bge-base or nomic-embed-text - same dimension family means in-place rollover, not schema migration |
| Query embedding cost (~10-20ms) per call dominates for trivial queries | Low | Low | Cache embeddings for recent queries in oracle if it becomes a measured problem. Skip for now |
| Conflict between cortex daemon embed job and ad-hoc `cortex embed` | Med | Low | File lock at `~/.local/share/cortex/embed.lock`; daemon and CLI both acquire it; second instance exits cleanly |

## Open Questions

Resolved during Round 1 of architect review (kept here as decisions of record):

- [x] **Where does `embed_query` actually live at process boundaries?** Resolved: function lives in `vault::embedding`; both oracle and cortex pull the same code. Oracle loads the model lazily on the first non-BM25 call; cortex loads eagerly at the start of each embed invocation. See Phase A2.
- [x] **Vector storage shape (`sqlite-vec` virtual table vs `BLOB` column).** Resolved during Round 2 conversation: BLOB column on a regular table. The design's RRF-in-Rust fusion never benefits from a virtual table's SQL-level KNN composition, and the BLOB shape eliminates the `unsafe` FFI transmute, the FK-CASCADE-through-virtual-table trigger, and the dim-locked-in-virtual-table migration headache. Cosine similarity is a 15-line pure-Rust function. See "Schema additions" and Phase A1.
- [x] **URL-kind verbatim recall (Video and Thread).** Resolved: Video and Thread both join the chunked-transcript group in Phase B2. The distillers amendment to render `## Transcript` for both kinds is part of Phase B2's work. See "What gets embedded per kind."

Still open:

- [ ] **fastembed first-load behavior:** verify whether fastembed-rs downloads the model from HuggingFace on first `TextEmbedding::try_new(...)` or whether the crate bundles weights. Affects `--prefetch-model` semantics. Resolve before Phase A8 documentation goes out.
- [ ] **Should the default `K_RRF_INPUT` be tuneable per query?** Pulling 50 from each list before fusion is a reasonable default but heavy queries may want 100. Leave as a constant for Phase A; revisit if regression fixture reveals systematic miss patterns.
- [ ] **Cosine vs L2 distance metric.** bge-small embeddings are L2-normalized at output, so cosine, inner-product, and (negative) L2 produce identical rankings for ranking purposes. Pure-Rust cosine is the simplest to implement and matches the standard expectation. Decision: implement cosine; document the normalization invariant so future model swaps don't silently break the assumption.
- [ ] **Test isolation strategy for fastembed.** Loading the real model in `cargo test --all` adds ~2s per test process. `MockEmbedder` handles unit tests, but integration tests need the real model. Use a `OnceCell<Arc<FastEmbedModel>>` at test-crate level so the model loads once per `cargo test` invocation.

## References

- [docs/scaling-roadmap.md](../scaling-roadmap.md) - Doc 2 (this doc's parent)
- [docs/design/2026-05-16-extractor-contract-and-l2-summaries.md](2026-05-16-extractor-contract-and-l2-summaries.md) - Doc 1, the L2 distilled contract this doc embeds
- [docs/design/2026-05-16-extractor-contract-l2-phase-9-cleanup.md](2026-05-16-extractor-contract-l2-phase-9-cleanup.md) - Phase 9 cleanup, introduces `## Transcript` for non-URL kinds
- [docs/design/2026-04-19-staged-ingestion-pipeline.md](2026-04-19-staged-ingestion-pipeline.md) - staged ingestion pipeline; Doc 2's re-embed loop runs *after* the pipeline produces distilled artifacts
- [docs/design/2026-04-20-sqlite-ledger-and-views.md](2026-04-20-sqlite-ledger-and-views.md) - SQLite ledger architecture; vector storage co-locates here
- [docs/design/2026-03-22-vault-watcher-oracle-reindex.md](2026-03-22-vault-watcher-oracle-reindex.md) - watcher → reindex; the staleness contract hooks into this path
- `vault/src/search.rs:202-389` - the existing FTS5 schema and triggers Phase A1 extends
- `vault/src/distilled.rs` - the L2 contract; `Distilled.summary` is the Phase A embedding target
- [fastembed-rs](https://github.com/Anush008/fastembed-rs) - Rust bindings for local ONNX embeddings
- [hnsw_rs](https://crates.io/crates/hnsw_rs) - pure-Rust HNSW; the documented future-scale upgrade path (sidecar) if brute-force latency becomes user-visible
- [Reciprocal Rank Fusion paper](https://plg.uwaterloo.ca/~gvcormac/cormacksigir09-rrf.pdf) - Cormack et al., 2009
