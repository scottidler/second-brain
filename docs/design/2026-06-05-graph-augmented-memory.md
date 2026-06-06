# Design Document: Graph-Augmented Memory for the Second Brain

**Author:** Scott A. Idler
**Date:** 2026-06-05
**Status:** Implemented
**Review Passes Completed:** 5/5 + Architect review rounds 1-3 (2026-06-05) + intent-layer code audit (2026-06-05)

## Summary

Evolve the second-brain from hybrid retrieval (BM25 + vector + RRF) into a graph-augmented memory layer, optimized for the agent (Claude) that consumes oracle over MCP. The work proceeds from a deterministic materialized edge graph and a comprehensive cortex linking pass, through auto-maintained entity hub notes, to a full **MemGraphRAG** terminal state: a three-layer (ontology / factual / passage) typed-edge graph maintained by cortex consolidation agents that dedup noise, resolve contradictions, and bridge disconnected clusters. Each phase ships standalone retrieval value; the deterministic phases make the later LLM/graph phases tractable and measurable rather than speculative.

## Problem Statement

### Background

The second-brain is three subsystems over one Obsidian vault and one SQLite index:

- **borg** ingests (Telegram/Signal/Discord/ntfy/HTTP/clipboard/extension), distills via Fabric, and writes markdown notes. ~15-20 notes/day and growing.
- **cortex** governs the vault: lint, link, tags/sweep, quality, embeddings (sole writer of `note_embeddings`).
- **oracle** retrieves over MCP: `knowledge_search` with `mode` ∈ {bm25, vector, hybrid (RRF, k=60)}, plus `find_links`, `domain_brief`, `creator_browse`, `source_browse`, and ~15 other tools.

Oracle already implements the "agentic hybrid retrieval" thesis that dominates the AI corpus in the vault (e.g. *Why the Best AI Coding Tools Abandoned RAG*, *Architectural patterns for graph-enhanced RAG*, the Pinecone "knowledge layer" talk, *MemGraphRAG (Outperforms Every RAG)*). What it lacks is the **relationship layer** those same notes argue is the next step: answers that "live between documents," multi-hop reasoning, and pre-assembled knowledge bundles instead of per-query rediscovery.

### Measured state of the vault (2026-06-05)

Numbers below come from parsing the vault directly, not from frontmatter labels:

- **1,356 notes.** Real note→note wikilinks (excluding `![[image]]` media embeds) exist in only **~25%** overall, and **13%** of borg-ingested notes.
- **~99%** of notes carry tags from the **110-term canonical vocabulary**, averaging **~5.6 of the 7-max** on the ingested half.
- `cortex link --scan all` (report mode) yields **52 candidate links across the entire vault**, nearly all inside a single design doc that enumerates note titles — **effectively zero on distilled article/video notes.**

The root cause is not a matching bug. The linker's `concepts` targets are *other notes' titles*, matched verbatim (`find_mention`, `min_word_length: 5`). Distilled article/video titles are long headlines that never appear inside other notes' bodies, so concept-linking structurally cannot fire across the ingested half. `entities.people`/`entities.projects` are curated lists that do not contain AI-domain entities. There is no general concept/entity vocabulary.

Critically: the vault is entirely machine-generated. The `authored` vs `assisted` origin split marks *tooling generations*, not human authorship — an earlier generator emitted more wikilinks but no canonical tags; current borg applies canonical tags and defers linking to cortex. So link density is a normalizable artifact, and the dense, governed substrate available *today* is the canonical-tag vocabulary plus near-universal metadata (`creator`, `source`, `domain`).

### Problem

Oracle returns ranked individual notes. It cannot expand from a hit to its related neighbors, cannot answer questions whose answer is distributed across several notes, and forces Claude to re-discover the same clusters every session. The wikilink graph that would enable this is sparse on the half of the vault that is actually growing.

### Goals

- Give oracle a **graph-expansion retrieval mode**: seed with BM25/vector, expand along edges, fuse with RRF.
- Make the graph **dense on the ingested half** using deterministic signals available today — primarily the embeddings cortex already writes (semantic kNN), with rarity-weighted tags and metadata as secondary bridges.
- Upgrade cortex linking into a **comprehensive pass** (concept glossary + piped aliases) so the vault's actual body wikilink graph densifies in both eras; handle creator/source relationships as derived edges, not YAML rewrites.
- Produce **entity hub notes** that double as human-navigable knowledge and machine-readable knowledge bundles.
- Reach **MemGraphRAG**: typed factual edges, a three-layer memory model, and consolidation agents that keep the graph clean (dedup, contradiction resolution, cluster bridging).
- Preserve **provenance** end to end (every edge and claim traceable to a source note).

### Non-Goals

- A human-facing knowledge-graph UI or browsing experience. The consumer is Claude over MCP; any human readability of hub notes is a welcome side effect, not a design driver.
- Replacing BM25/vector retrieval. The graph augments fusion; it does not supplant the existing lists.
- A parametric memory model (MeMo-style). It needs ~240 GPU-hours and, per its own note, "obscures the provenance of information." Provenance is load-bearing here; this approach is explicitly rejected.
- An external graph database (Neo4j). The graph lives in the SQLite index oracle already owns; introducing a second store violates the one-index invariant.
- Changing borg's ingestion contract or the distiller output schema.
- **Any dependency requiring newer hardware than desk.lan's CPU.** The daemon host has an older CPU. The design is pinned to the stack already running there — fastembed (`bge-small-en-v1.5` via ONNX), brute-force cosine, SQLite, env_logger/log — and adds no native library that assumes modern SIMD (AVX-512), and no GPU. Phase 5 LLM extraction runs through the existing remote Fabric path, not local inference, so it adds no CPU load on desk.lan.

## Proposed Solution

### Overview

Five phases, each independently shippable, forming one arc:

1. **Materialized edge graph + graph-expansion retrieval** — deterministic edges (semantic-kNN over existing embeddings as the primary discriminating edge; wikilink; rarity-weighted shared-tag; metadata-derived) built by a cortex pass into oracle's SQLite index; an oracle `graph` retrieval path that seeds then expands one hop and fuses via RRF.
2. **Comprehensive cortex linking** — a concept glossary and an alias table (piped links), so the vault's real wikilink graph densifies in note bodies (feeding Phase 1's `wikilink` edge class). Metadata relationships are handled as derived edges in Phase 1, not as YAML rewrites.
3. **Entity hub notes** — auto-stubbed `[[entity]]` / `[[creator]]` / `[[source]]` notes that resolve the new links and serve as pre-assembled knowledge bundles (the Pinecone pattern, and the LLM-Wiki entity page).
4. **LLM entity discovery** — an off-hot-path cortex pass that proposes new glossary entries from distilled notes into `entity-proposals.yml`, mirroring `tag-proposals.yml`. Grows the vocabulary; never links inline.
5. **MemGraphRAG** — typed factual edges (subject-predicate-object), the three-layer memory model (ontology / factual / passage), and consolidation agents for noise removal, contradiction resolution, and cluster bridging. Retrieval becomes relationship-aware.

Phases 1-3 are fully deterministic and reuse existing machinery. Phase 4 introduces the only LLM cost and confines it to vocabulary growth. Phase 5 is the recommended terminal state and is where typed relationships and active curation earn their keep.

### Architecture

```
borg (ingest, distill, write notes + tags + metadata)
        │
        ▼  vault (markdown: body wikilinks, frontmatter tags/creator/source/domain)
        │
cortex ─┼─ embed  (existing: writes note_embeddings)
        ├─ graph  (Phase 1: deterministic edges — runs AFTER embed; Phase 5: typed triples + consolidation)
        ├─ link   (Phase 2: glossary + aliases → writes [[wikilinks]] into note BODIES only)
        ├─ hub    (Phase 3: auto-stub entity/creator/source hub notes)
        └─ entities --discover (Phase 4: LLM → entity-proposals.yml)
        │
        ▼  SQLite index (cortex writes, oracle reads — one index, existing invariant)
            notes, notes_fts, note_embeddings, embedding_config   (existing)
            edges  (semantic-kNN from note_embeddings + wikilink   (new, Phase 1)
                    + rarity-weighted shared-tag + metadata-derived)
            entities                                              (new, Phase 3/5)
        │
oracle ─── knowledge_search mode=graph | graph-hybrid (Phases 1, 5) — READS edges, never builds them
            find_links / domain_brief / creator_browse (existing, now graph-backed)
```

**Edge construction lives in a cortex pass (`sb cortex graph`), not in oracle's `index_vault`.** This is a deliberate correction over an earlier draft and resolves two coupled problems:

1. **Embedding dependency.** `note_embeddings` are written asynchronously by `cortex embed` *after* `index_vault` records a note. If `index_vault` built semantic edges it would find no embedding for a freshly ingested note and emit zero semantic edges on day one. Building edges in a cortex pass that runs *after* `embed` guarantees the vectors exist first.
2. **Lock contention.** `index_vault` runs in oracle's synchronous `VaultWatcher` path holding the `Mutex<SearchIndex>`; the watcher fires sub-second on every save. O(N²) cosine + tag-bucket work there would starve concurrent `note_read`/`knowledge_search` — the exact anti-pattern oracle already avoids by running inbound-link recompute as a decoupled 10-minute background task (`oracle/src/lib.rs`). The cortex graph pass runs on its own cadence (like `cortex embed`), wrapped in `block_in_place` per the workspace's daemon convention, and writes `edges` in bounded transactions.

cortex is already the sole writer of `note_embeddings` into oracle's SQLite, so writing `edges` into the same file preserves the one-index invariant; oracle only ever reads `edges`.

### Data Model

**New `edges` table (Phase 1), materialized by the `sb cortex graph` pass (after `cortex embed`):**

```sql
CREATE TABLE IF NOT EXISTS edges (
    src        TEXT NOT NULL,        -- vault-relative path of source note
    dst        TEXT NOT NULL,        -- vault-relative path of target note. ALWAYS a row in `notes`
                                     -- at insert time (the dst FK rejects otherwise): entity edges
                                     -- target the entity's HUB note path (stubbed first), and
                                     -- wikilink edges are emitted only for RESOLVED targets
                                     -- (danglers skipped). Never a bare entity id. See below.
    kind       TEXT NOT NULL,        -- 'semantic' | 'wikilink' | 'shared-tag'
                                     -- | 'shared-creator' | 'shared-source' | 'shared-domain'
                                     -- | (Phase 5) 'fact'  (relation carried in `predicate`)
    weight     REAL NOT NULL,        -- edge strength (see weighting below)
    predicate  TEXT NOT NULL DEFAULT '', -- '' for deterministic kinds; set for Phase-5 typed edges
                                     -- (NOT NULL: SQLite treats NULLs as distinct in a PK, which
                                     -- would defeat dedup of deterministic edges)
    src_note   TEXT NOT NULL DEFAULT '', -- provenance: note the edge was derived from (Phase 5)
    PRIMARY KEY (src, dst, kind, predicate),
    -- Self-cleaning, mirroring note_embeddings' cascade: when oracle's index_vault drops a
    -- deleted note from `notes`, its incident edges vanish natively, so traversal never
    -- surfaces a path that no longer exists.
    FOREIGN KEY (src) REFERENCES notes(path) ON DELETE CASCADE,
    FOREIGN KEY (dst) REFERENCES notes(path) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_edges_src ON edges(src);
CREATE INDEX IF NOT EXISTS idx_edges_dst ON edges(dst);
CREATE INDEX IF NOT EXISTS idx_edges_kind ON edges(kind);
```

The `ON DELETE CASCADE` is load-bearing and mirrors the existing `note_embeddings` foreign key: oracle's `index_vault` is the sole **deleter** of `notes` rows (the only caller of `remove_stale_notes`) and never touches `edges`, so without the cascade a deleted note would leave orphaned edges that traversal surfaces as phantom paths (Claude would cite files that don't exist). (Note: `index_vault` is not the sole *writer* of `notes` — the per-note upsert `index_one` is reached by both the full walk and the `VaultWatcher` mtime path, and `bump_access` / `recompute_inbound_link_counts` also UPDATE `notes` — but none of those *delete*, so the cascade contract holds.)

Both `src` and `dst` can cascade only because **`dst` is always a real note path that exists in `notes` at insert time**. The `dst` FK rejects any insert whose target is absent, which would abort the entire graph transaction, so the graph pass enforces **one universal invariant for every edge it writes, of every kind**:

> **Resolve-`dst`-or-skip.** Before inserting any edge, the pass checks that `dst` exists in `notes`; if it does not, the edge is silently skipped (and logged), never inserted.

This single rule subsumes every per-kind case and is the correct generalization of the dangling-wikilink fix:

1. **`wikilink` edges** — `extract_wikilinks` yields slugs verbatim, and dangling wikilinks are common in the vault (`resolve_wikilink` returns `Option`; many `OutboundLink` rows carry `exists: false`). Danglers are skipped. This is consistent with traversal semantics: a phantom `dst` is exactly the hallucination vector the cascade exists to prevent, so refusing to create it at build time is correct, not a compromise.
2. **entity / hub edges (Phase 3/5)** — these point at the entity's *hub note* path, and hub notes are stubbed (Phase 3) **before** their edges are created, never at a bare entity id. But a hub note can be deleted out-of-band (a human `rm`s it in the vault); `VaultWatcher` then drops it from `notes` and the cascade clears its incident edges, yet `entities.hub_path` still holds the now-stale path. Without resolve-`dst`-or-skip, the next graph pass would try to re-insert edges to that vanished hub and the FK would abort the whole transaction. The universal rule makes this a skip, not a crash; `cortex hub` re-stubs the hub on its next sweep and the edges return.
3. **Phase-5 `fact` edges** — same rule: a typed triple whose object resolves to a missing note path is skipped, not inserted.

The cascade keeps the table clean *on delete*; resolve-`dst`-or-skip keeps inserts from ever crashing *on a stale or unresolved target*. The two together are what make the `dst` FK safe.

This requires the connection to run `PRAGMA foreign_keys=ON` (SQLite defaults it off per-connection); the codebase already relies on this for the `note_embeddings` cascade, so the pragma is set on open in `vault/src/search.rs` (`search.rs:203` for `open`, `:215` for `open_memory`).

Deterministic edge construction (Phase 1), all derived from data already in the index — built by the cortex graph pass after embeddings are current:

- **semantic** (primary discriminating edge): each note's top-`k` cosine neighbors over `note_embeddings`, above a `min_cosine` threshold; `weight = cosine`. This is the edge that actually carries topical relatedness; it requires no LLM and no new vocabulary. Because it reads `note_embeddings`, it only runs for notes whose embedding exists — the pass skips (and logs) notes still awaiting `cortex embed`. They are picked up once their embedding lands because semantic-edge selection is keyed on embedding freshness (`note_embeddings.produced_at`), **not** on `notes.modified_at` (see the decoupled-triggers requirement below — keying on `modified_at` would strand them permanently). Undirected (kNN is asymmetric, so stored both directions when either lists the other). **Implementation note:** the existing `search_vector(query_vec, …)` takes an *external* query vector, and the per-note BLOB decoder (`dot_product_from_bytes`) is private, so there is no public "give me this note's stored vector / its neighbors" path today. Phase 1 adds a `vault::search` helper that, for a given note path, reads its `note_embeddings` BLOB and returns its top-`k` neighbors (reusing the same brute-force dot-product loop `search_vector` already runs); the graph pass calls that helper rather than re-deriving cosine inline.
- **wikilink** (high-confidence, sparse today, densified by Phase 2): from `extract_wikilinks(body)`; `weight = 1.0`. Directed src→dst; expansion traverses both directions. `extract_wikilinks` is currently **private** (`fn`, not `pub fn`) in `search.rs` — Phase 1 exposes it (regex `r"\[\[([^\]|#]+)(?:[|#][^\]]+)?\]\]"`, captures the slug and discards `|alias` / `#heading`). Only **resolved** targets (a matching row in `notes`) become edges; dangling wikilinks are skipped per the `dst` FK discipline above.
- **shared-tag** (weak clustering signal only): for note pairs sharing tags, weighted by tag **rarity**, not raw overlap — `weight = Σ_{t ∈ shared} 1/log(1 + df_t)`, so a shared blanket tag (`llm`, df≈437) contributes ~nothing while a shared rare tag is discriminating. Plain Jaccard is explicitly **not** used (validation below). Undirected.
- **shared-creator / shared-source / shared-domain** (metadata-derived): pairs with identical `creator` / source-host / `domain`, read **directly from frontmatter** at graph-build time; low fixed weight (e.g. 0.2/0.15/0.1), undirected. No note content is rewritten to produce these — they are pure derivations from existing fields (see Phase 2 note on why YAML is never mutated).

Self-edges (`src == dst`) are never emitted. The pass rebuilds edges incident to changed notes only (delete-then-insert by `src`), not a full-table rebuild each cadence.

**Two decoupled incremental triggers (do NOT key everything on `notes.modified_at`).** This is a correctness requirement, not an optimization. `notes.modified_at` is the filesystem mtime, written only by `index_one` when `VaultWatcher` sees a save; `cortex embed` writes `note_embeddings.{produced_at, source_modified_at}` **asynchronously later and never touches `notes.modified_at`** (verified: `upsert_embeddings_batch`, vector.rs:283). So a single moving `modified_at` watermark **strands notes**: a note edited at t0 is seen by the graph pass at t1 but skipped (no embedding yet); the watermark advances to t1; `cortex embed` lands the vector at t2 without moving `modified_at` (still t0); the next pass at t3 evaluates `modified_at(t0) > watermark(t1)` as false and **never revisits the note for semantic edges**. It is permanently stranded until the file is edited again. Therefore:

- **semantic edges** are driven off embedding freshness, mirroring how `cortex embed` itself selects work — a per-row staleness comparison (`note_embeddings.produced_at` newer than the note's last semantic-edge build), not the `modified_at` watermark. This is the same pattern `stale_embedding_targets` (vector.rs:417-439) already uses (`e.source_modified_at < n.modified_at`) and is why embed never strands a note.
- **wikilink / shared-tag / metadata edges** are driven off `notes.modified_at` (indexed at `idx_notes_modified_at`); these derive purely from note content/frontmatter that `index_one` captures synchronously, so `modified_at` is the right trigger for them.

**Restart lifecycle.** The incremental high-water mark must survive daemon restarts or edits made while cortex was down would be missed. The graph pass persists its last-run timestamp (not in-memory only) and, on first run after start, does a **full rebuild** before resuming incremental cadence. A full rebuild is O(N²) cosine but bounded (seconds at ~1,400 notes) and structurally safe, so a cold start is never wrong, only briefly more expensive. **Where the watermark lives:** *not* in `embedding_config` — that table is feature-gated (`#[cfg(feature = "vec")]`) and exposes only key-specific accessors (`active_embedding_model` / `_dim` / `set_active_embedding`), no generic KV get/set. Phase 1 instead creates a small **always-present** `graph_state` table (`key TEXT PRIMARY KEY, value TEXT`) in `ensure_schema` alongside `edges`, or reuses cortex's existing file-based state pattern (`cortex::state::VaultManifest::{save,load}`). The SQLite table is preferred so the watermark lives in the same file as the edges it guards.

**Validation (2026-06-05) — why semantic, not tags, is primary.** The 108-term canonical vocabulary is too coarse to be the relatedness substrate: `llm` blankets 32% of the corpus, `claude` 21%, `agents` 20%. A plain-Jaccard shared-tag graph for the MemGraphRAG seed returns generic AI notes (`agents-that-remember`, `agents-need-vms-not-containers`, a GLM-5 release) that share only blanket tags and have no topical relation to graph memory — a hairball that would *degrade* retrieval. Tags are excellent for *faceting/filtering* (the existing `domain`/`note_type`/`status` filters) but poor for fine-grained edges; embeddings already encode the discriminating signal, so they lead. Rarity-weighting keeps shared-tag as a useful bridge for the rare, meaningful tags without the blanket-tag noise.

Semantic-kNN is the densifier on the ingested half; wikilink is the high-confidence edge that Phase 2 grows; shared-tag is a secondary bridge. To bound shared-tag construction at 1,356+ notes, edges are built via a **tag→notes inverted index** (group notes by tag, emit pairs within each tag bucket, accumulate the rarity-weighted sum), not a full pairwise scan. Buckets above a fan-out cap (a tag held by hundreds of notes) are skipped for pairwise emission and instead routed through the tag's hub note (Phase 3), which keeps the dense `llm`/`ai` tags from exploding the table.

**New `entities` table (Phase 3, extended in Phase 5):**

```sql
CREATE TABLE IF NOT EXISTS entities (
    id        TEXT PRIMARY KEY,      -- slug, e.g. 'graphrag'
    kind      TEXT NOT NULL,         -- 'concept' | 'person' | 'project' | 'creator' | 'source'
    hub_path  TEXT,                  -- vault path of the hub note, if stubbed
    ontotype  TEXT                   -- (Phase 5) ontology class, e.g. 'technology' | 'organization'
);
```

**Config additions (Phase 2), in `cortex/src/config.rs`:**

- `LinkingEntities` gains `concepts: Vec<String>` (loaded from a shared `config/glossary.yml`, kebab-case keys, mirroring `canonical-tags.yml`).
- `LinkingConfig` gains `aliases: HashMap<String, String>` (alias surface form → canonical entity slug). When `find_mention` matches an alias, the linker emits a **piped wikilink** `[[rag|Retrieval-Augmented Generation]]` — preserving the displayed prose while pointing at the canonical slug. Two signatures change, not one:
  - `insert_first_wikilink(content, target)` currently wraps the matched text verbatim as `[[matched]]` (which for an alias would produce a dangling `[[Retrieval-Augmented Generation]]`). It gains a third arg — the surface text — and emits `[[target|surface]]` when target ≠ surface, `[[target]]` otherwise.
  - `find_mention` currently returns only the match *context* string, not the matched surface text the piped link needs. It must also return the matched surface form (widen its return to carry both), so the linker can thread that surface text into `insert_first_wikilink`.
- **No metadata linking into frontmatter.** Creator/source relationships become `edges` derived from the existing `creator`/`source` fields (above) and hub notes (Phase 3) — the linker never writes `creator: "[[…]]"` into YAML. `insert_first_wikilink` already skips frontmatter on purpose (to avoid corrupting YAML); the machine-facing goal needs an edge, not a rewritten field, so there is nothing to inject.

### API Design

**Oracle `KnowledgeSearchRequest` (`oracle/src/tools.rs`)** — extend `SearchMode`:

```rust
pub enum SearchMode {
    Bm25,
    Vector,
    Hybrid,        // existing default
    Graph,         // seed via hybrid, expand one hop along edges, re-rank
    GraphHybrid,   // fuse graph-expanded set into RRF alongside bm25/vector
}
```

New optional request fields (all default to current behavior when omitted):

- `expand_hops: Option<u8>` (default 1; capped at 2)
- `edge_kinds: Option<Vec<String>>` (default: all deterministic kinds; Phase 5 adds typed predicates)
- `min_edge_weight: Option<f32>` (default 0.0)

**Retrieval algorithm (`mode = graph-hybrid`):**

1. Seed: run existing hybrid (BM25 ∪ vector, top `K_RRF_INPUT` each).
2. Expand: for each seed note, pull neighbors from `edges` where the seed appears as **either** `src` or `dst` (`src IN seeds OR dst IN seeds`), `weight ≥ min_edge_weight`, and `kind ∈ edge_kinds`, up to `expand_hops`. Every neighbor is a real note path (including entity hub notes), so all expand into the result set — there are no unresolvable entity-id neighbors to filter. **The `edges` read lives in `vault::search`, not oracle:** oracle cannot add methods to the shared `vault` crate and is a thin reader, so Phase 1 adds `vault::search::SearchIndex::expand_graph(seed_paths, hops, edge_kinds, min_weight) -> Vec<…>` (a single indexed lookup per seed over `idx_edges_src`/`idx_edges_dst`), and oracle calls it — mirroring how `find_links` already delegates to `find_outbound_links` / `find_inbound_links` rather than querying inline.
3. Score expanded notes, then **convert to a rank list** — RRF consumes ranked lists, not raw scores (`reciprocal_rank_fusion` takes `&[String]` path lists ordered by rank). For each expanded note compute `expansion_score = Σ_over_paths_reaching_it ( w_seed(seed) · edge_weight · decay^(hop-1) )`, where `w_seed` is `1/(K_RRF_INPUT − seed_rank)` (the seed's own standing) and `decay ∈ (0,1]` (default 0.5, one effective hop). Sort expanded notes by `expansion_score` descending to produce the `graph` rank list. The continuous scores never enter RRF — only the resulting order does.
4. Fuse the seed list and the graph rank list via `reciprocal_rank_fusion` (reuse, `RRF_K = 60`). `mode=graph` returns the graph list alone (re-fused with the seed list); `mode=graph-hybrid` additionally carries the original BM25 and vector lists into the fusion.
5. Filters and limit follow the existing hybrid ordering — **filters first, limit last**, not "filters last." Schema filters (`domain`/`note_type`/`status`) are pushed into the seed `BM25`/`vector` SQL at step 1 (so the seed set is already filtered); expanded neighbors from step 2 are filtered against the same predicates before scoring (a neighbor in a different domain is dropped); `limit` truncates the fused list at the very end.

**cortex CLI (`sb cortex …`):**

- `sb cortex link` gains glossary/alias/metadata behavior (no new verb; `--scan metadata` added).
- `sb cortex hub [--apply]` (Phase 3): stub/refresh entity, creator, and source hub notes.
- `sb cortex entities --discover` (Phase 4): emit `entity-proposals.yml`.
- `sb cortex graph` (Phase 1): build deterministic edges; daemon picks it up on a cadence after `cortex embed`. Extended in Phase 5 with `--backfill` to extract typed triples and run consolidation.

### Implementation Plan

#### Phase 1: Materialized edge graph + graph-expansion retrieval
**Model:** opus
- Add `edges` table + the three indexes **and** the `graph_state` KV table to the single schema-creation function `ensure_schema()` in `vault/src/search.rs` (search.rs:222, called by both `open()` and `open_memory()` — one edit covers disk and in-memory test paths). Plain (non-FTS) tables, so no trigger drop/recreate ceremony; no feature gate (the tables exist in both `vec` and non-`vec` builds). They are created at schema-init; `edges` is **populated by the cortex graph pass, not by `index_vault`**.
- Expose `extract_wikilinks` as `pub` (currently private at search.rs:1746) and add a `vault::search` neighbor helper for the kNN edge: given a note path, read its `note_embeddings` BLOB and return top-`k` neighbors ≥ `min_cosine`, reusing the existing brute-force dot-product loop (today only `search_vector(query_vec, …)` and the private `dot_product_from_bytes` exist — there is no per-note vector reader). Add `vault::search::SearchIndex::expand_graph(seed_paths, hops, edge_kinds, min_weight)` for the oracle read side (single indexed lookup per seed).
- Add `sb cortex graph` (new cortex module + verb): build deterministic edges incrementally with **two decoupled triggers** (a single `notes.modified_at` watermark strands notes whose embedding lands after they were skipped — see Data Model): **semantic-kNN** keyed on embedding freshness (`note_embeddings.produced_at`, mirroring `stale_embedding_targets`; the vault neighbor helper above; skip+log notes with no embedding yet); **wikilink** / **rarity-weighted shared-tag** / **metadata-derived** keyed on `notes.modified_at` (indexed at `idx_notes_modified_at`). Wikilink edges are **resolved targets only**, and in fact **every** edge insert obeys the universal resolve-`dst`-or-skip rule (skip any edge whose `dst` is absent from `notes` so the `dst` FK can never abort the batch). Shared-tag via a tag→notes inverted index (weight `Σ 1/log(1+df)`, fan-out cap on blanket tags); metadata shared-creator/source/domain read straight from frontmatter. The pass opens its **own** `SearchIndex` connection (cortex commands do not share a `Mutex`; each opens `config.oracle_db_path()`), serializes against concurrent `cortex embed` via the **existing `acquire_lock()` file lock** embed already uses, writes in bounded transactions, persists its last-run timestamp to `graph_state`, and on first run after a restart does a full rebuild. Runs in the daemon wrapped in `block_in_place`, on its own `graph_interval_secs` cadence (new `DaemonConfig` field) ordered after `cortex embed`.
- Extend oracle `SearchMode` with `Graph`/`GraphHybrid` and the three optional `KnowledgeSearchRequest` fields (`expand_hops`/`edge_kinds`/`min_edge_weight`, matching the existing `#[derive(… Deserialize, JsonSchema)]` + kebab-case pattern); add two match arms after `Hybrid` in the `knowledge_search` dispatch (server.rs:259) that call `expand_graph` then reuse `reciprocal_rank_fusion`. Oracle reads `edges` via the vault method; it never writes them.
- Tests: semantic-kNN edge construction (threshold/k honored, missing-embedding skip), rarity weighting downweights blanket tags, fan-out cap behavior, expansion bounded by hops/weight, score→rank conversion + decay, RRF fusion of the graph list, schema-filter interaction. **Watermark regression:** a note skipped for a missing embedding gets its semantic edges once the embedding lands (no stranding) — the test edits a note, runs the pass (skip), upserts the embedding *without* bumping `notes.modified_at`, runs the pass again, and asserts the semantic edges now exist. **Resolve-`dst`-or-skip:** an edge whose `dst` is absent from `notes` (dangling wikilink) is skipped, not inserted, and the batch does not abort.

#### Phase 2: Comprehensive cortex linking
**Model:** opus
- Add `concepts: Vec<String>` to `LinkingEntities`; load `config/glossary.yml` (and ship a starter via `sb bootstrap`, mirroring `canonical-tags.yml`).
- Add `aliases: HashMap<String,String>` to `LinkingConfig` (`#[serde(default)]`). Extend `insert_first_wikilink(content, target)` with a third surface-text arg and emit a **piped link** `[[slug|surface text]]` when target ≠ surface, `[[target]]` otherwise. **Also widen `find_mention`'s return** to carry the matched surface text (it currently returns only the match context), so the linker can thread that surface text into `insert_first_wikilink`; thread the alias map through `find_mention`. Add a `Metadata` variant to the `ScanScope` enum (`cortex/src/opts.rs`) and its `as_config_scan_for()` mapping for the new `--scan metadata`.
- Seed `config/glossary.yml` with the AI/tech entity vocabulary already evidenced in the vault (RAG, GraphRAG, Cognee, Neo4j, LangChain, Pinecone, tree-sitter, Claude Code, Anthropic, RRF, fastembed, …) plus an `aliases` block (`Retrieval-Augmented Generation → rag`, …).
- **No frontmatter mutation:** creator/source relationships are the metadata-derived edges from Phase 1, not YAML rewrites. `insert_first_wikilink` continues to skip frontmatter.
- Tests: glossary mentions link in body, aliases emit correct `[[slug|surface]]` piped links, frontmatter never modified, no double-linking, existing concept/people/project behavior preserved.

#### Phase 3: Entity hub notes
**Model:** sonnet
- `sb cortex hub [--apply]`: for each glossary concept / distinct creator / distinct source-host, stub a hub note if absent (frontmatter `note_type: entity`, `ontotype` where known), idempotent refresh otherwise.
- Populate the `entities` table in the cortex graph pass from glossary + observed creators/sources; set `hub_path` when a hub note exists. Entity/hub edges obey the universal resolve-`dst`-or-skip rule (Data Model): if a hub note was deleted out-of-band and `entities.hub_path` is now stale, edges to it are skipped (logged), never inserted — so a vanished hub never aborts the graph transaction; `cortex hub` re-stubs it on the next sweep and the edges return.
- Route over-cap shared-tag buckets (Phase 1) through tag hub notes so dense tags become hub-mediated edges rather than pairwise explosions.
- Tests: hub stub creation idempotent, frontmatter correct, `entities` table populated, deletion-safe via `rkvr` (no `rm`). **Out-of-band hub deletion:** stub a hub, build its entity edges, delete the hub note (cascade clears edges, `entities.hub_path` left stale), re-run the graph pass — assert it skips the stale-`dst` edges without error and `cortex hub` re-stubs the hub.

#### Phase 4: LLM entity discovery
**Model:** opus
- `sb cortex entities --discover`: over ingested notes (origin filter, per the ingested-only convention), run an extraction prompt to propose entities absent from the glossary; write `config/entity-proposals.yml` (mirrors `tag-proposals.yml`, never auto-promotes).
- Daemon cadence option, bounded concurrency (respect the no-unbounded-fanout rule).
- Tests: proposals exclude existing glossary entries, output schema stable, ingested-only scoping.

#### Phase 5: MemGraphRAG
**Model:** opus

**Where the value actually concentrates for this corpus.** MemGraphRAG's three consolidation agents were designed for large, noisy, multi-source auto-extracted graphs. Against ~1,400 single-curator, tag-governed notes the value is uneven, and that should be stated plainly: **cluster-bridging is genuinely valuable** (the islands are measured and real); **noise removal is moderate**; **contradiction resolution is near-idle** — this corpus is largely *opinion* ("RAG is dead" and "RAG is essential" are both valid stances, not a conflict to reconcile), and the Pass-4 restriction to functional predicates means the contradiction agent rarely fires here. It is specced for completeness and future heterogeneous sources, not because it pays off heavily on today's vault. Build it; don't expect it to be the win.
- **Factual layer:** `sb cortex graph [--backfill]` extracts typed subject-predicate-object triples from distilled notes (entities resolved against the glossary/`entities` table); writes typed rows into `edges` with `kind = 'fact'`, the relation in `predicate`, and the originating note in `src_note` for provenance. Deterministic edges keep their `kind` (`wikilink`/`shared-tag`/…) and empty `predicate`, so the two layers never collide in the PK. Write transaction bounded like `cortex embed`; extraction runs outside the transaction.
- **Ontology layer:** `ontotype` on `entities`, seeded from `vault::schema` + glossary classes; enforced two-way relationships (entity↔fact↔passage) per the MemGraphRAG note.
- **Consolidation agents** (cortex daemon passes):
  - *Noise removal* — drop low-salience extracted facts (the "patient prefers tea" class).
  - *Contradiction resolution* — for predicates declared **single-valued/functional** (e.g. `born-in`, `released-on`), detect conflicting objects across notes; flag and reconcile by recency/source-confidence; record the conflict, never silently overwrite. Multi-valued predicates (`uses`, `mentions`) accumulate objects and are never treated as conflicts.
  - *Cluster bridging* — connect disconnected components via type-based bridges (shared `ontotype`) and embedding-similarity bridges (reuse `note_embeddings`).
- **Retrieval:** `mode=graph-hybrid` gains relationship-aware ranking — typed-edge traversal weighted by predicate relevance; `edge_kinds` accepts predicates.
- Tests: triple extraction provenance preserved, contradiction detection on a fixture, bridge construction across a synthetic island, retrieval improvement on a labeled query set vs. Phase 1 baseline.

## Alternatives Considered

### Alternative 1: Wikilink-only graph (no tag edges)
- **Description:** Build the graph purely from body wikilinks; rely on cortex linking to densify.
- **Pros:** Conceptually clean; one edge type; matches the LLM-Wiki framing.
- **Cons:** 13% wikilink coverage on the ingested half today; even with Phase 2, day-one coverage is poor and back-linking 500 notes/month never fully catches up.
- **Why not chosen:** Strands the growing half as islands until linking matures. Semantic-kNN edges (from embeddings that already exist) give discriminating density immediately; wikilinks become a high-confidence overlay as Phase 2 lands.

### Alternative 2: External graph database (Neo4j), à la Cognee
- **Description:** Stand up Neo4j; mirror the vault into it; query graph + vector there.
- **Pros:** Native multi-hop traversal; mature tooling; matches several corpus references.
- **Cons:** Second datastore to run, sync, and back up; violates the one-index invariant; another failure mode and deploy surface for a single-operator system.
- **Why not chosen:** SQLite already holds notes + FTS5 + embeddings; an `edges` table there gives sufficient traversal at this scale (low thousands of notes) without a new service.

### Alternative 3: Parametric memory model (MeMo)
- **Description:** Train a small memory model on reflection QA pairs; synthesize answers from parametric memory.
- **Pros:** Strong multi-document synthesis; robust to irrelevant-document flooding.
- **Cons:** ~240 GPU-hours to build; obscures provenance (synthesizes rather than cites); poor fit for a daily-growing personal corpus.
- **Why not chosen:** Provenance is a core requirement; MeMo trades it away. Rejected in Non-Goals.

### Alternative 4: LLM extraction from day one (skip deterministic phases)
- **Description:** Go straight to typed-triple extraction for every note at ingest.
- **Pros:** Richest edges immediately.
- **Cons:** Recurring LLM cost on 15-20 notes/day forever; hallucinated edges with no deterministic floor to compare against; no way to measure whether typed edges beat tag/wikilink edges on top of existing embeddings.
- **Why not chosen:** The deterministic phases provide a measurable baseline and a free, governed substrate. LLM extraction (Phases 4-5) layers on top once its marginal value can be observed.

## Technical Considerations

### Dependencies
- Internal: `vault::search` (schema, indexing, RRF, `extract_wikilinks`), `vault::schema`, cortex `linking`/config, oracle `tools`/`server`, `note_embeddings` (reused for Phase 5 bridging). No new crates anticipated; `cargo add` if extraction needs a structured-output helper.
- External: existing Fabric/LLM path for Phases 4-5 only.

### Performance
- Graph traversal raises *retrieval* latency on the oracle read path (the graph-enhanced-RAG note cites ~50-100ms → ~200-500ms with hops). One-hop default and `min_edge_weight` keep expansion bounded; the `edges` table is indexed on `src`/`dst`/`kind`. The `expand` query is a single indexed lookup per seed, not a scan.
- Edge *construction* is off oracle's hot path entirely: it runs in the cortex graph pass on a cadence (after `cortex embed`), wrapped in `block_in_place` like the existing cortex sweep, building only edges incident to notes changed since the last run (tracked via the two decoupled triggers — `note_embeddings.produced_at` for semantic edges, `notes.modified_at` for the rest — per Data Model). It never holds oracle's `Mutex<SearchIndex>` during a watcher tick.
- Semantic-kNN construction is an all-pairs cosine scan (O(n²·d)) for a full backfill; at ~1,400 notes with a 384-dim model this is the same brute-force math oracle already runs per vector query, and incremental runs only scan changed notes against the corpus. Brute force is the deliberate choice and the permanent plan — **no ANN/HNSW/FAISS-style index**, which would pull in SIMD/AVX-512-hungry native libraries that desk.lan's older CPU cannot rely on. If n ever genuinely outgrows brute force, the mitigation is a cheap pre-filter (same domain/recent window) to cap candidates, not a new vector-index dependency.
- Shared-tag construction is O(Σ bucket²) over tags, not O(n²) over notes, with a fan-out cap; rarity weighting plus the cap keep blanket tags (`llm`, 437 notes) from dominating the table.
- Phase 5 write transactions stay bounded like `cortex embed` (inference outside the transaction).

### Security
- No new external surface. Hub-note creation and triple extraction write inside the vault and index already owned by the user. Deletes (hub refresh, stale-edge cleanup) route through `rkvr` per the safety rule.

### Testing Strategy
- Unit tests per phase as listed, using in-memory SQLite and `tempfile` mini-vaults.
- A small labeled query set (questions whose answers span multiple notes) is the regression harness from Phase 1 onward. It *quantifies* the retrieval lift each phase adds (deterministic graph vs. typed edges); it is a measurement and regression guard, not a ship-gate — all phases land back-to-back.

### Rollout Plan
- Each phase ships behind its own additive surface (new `SearchMode` variants default off; new cortex verbs opt-in). Existing `hybrid` retrieval is untouched until a caller selects a graph mode.
- Deploy via the standard `bump && otto deploy`; cortex daemon picks up Phases 4-5 cadence after install. Per the no-phase-gating convention, the phases are implemented back-to-back; the labeled query set provides the empirical check, not a soak period.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Shared-tag edges form a non-discriminating hairball (confirmed: `llm`=32%, blanket-tag seeds return unrelated notes) | High | High | Semantic-kNN (not tags) is the primary edge; shared-tag is rarity-weighted (`1/log(1+df)`) so blanket tags ~vanish; inverted-index + fan-out cap + hub routing for the rest |
| Graph expansion floods results with weak neighbors | Med | Med | `min_edge_weight`, one-hop default, hop decay, RRF (rank- not score-based) |
| Phase-2 glossary linking over-links common words | Med | Med | `min_word_length`, curated glossary (not free text), alias map, existing-link dedup |
| LLM triple extraction hallucinates edges | Med | High | `src_note` provenance on every typed edge; contradiction agent; deterministic edges remain the floor |
| Typed edges add no retrieval lift over tags + embeddings | Med | Med | Labeled query set quantifies Phase 5 lift; deterministic phases deliver standalone value regardless |
| Latency regression on `graph` modes | Med | Low | Default mode stays `hybrid`; graph modes opt-in; indexed edges, bounded hops |
| Deleted note leaves orphaned edges → traversal cites phantom files | Med | High | `FOREIGN KEY (src/dst) REFERENCES notes(path) ON DELETE CASCADE` (pragma already on; mirrors `note_embeddings`) cleans edges *on delete* |
| Edge insert to an absent/stale `dst` aborts the whole graph transaction (dangling wikilink; out-of-band-deleted hub note whose `entities.hub_path` is now stale; unresolved Phase-5 fact object) | Med | High | **Universal resolve-`dst`-or-skip rule:** every edge insert checks `dst` exists in `notes` and skips (logs) if not, of every kind. Generalizes the dangling-wikilink fix; covered by a Phase-1 test and a Phase-3 hub-deletion test |
| Semantic edges stranded: a single `notes.modified_at` watermark never revisits a note whose embedding lands after it was skipped (embed does not bump `modified_at`) | Med | High | **Two decoupled triggers:** semantic edges keyed on `note_embeddings.produced_at` (per-row staleness, mirroring `stale_embedding_targets`); wikilink/tag/metadata keyed on `notes.modified_at`. Full rebuild on restart is the backstop |
| Daemon restart drops the incremental high-water mark | Low | Med | Persist last-run timestamp in key/value state; full rebuild on first run after start (bounded, seconds) |

## Open Questions
- [ ] Semantic-kNN `k` and `min_cosine` threshold, and per-kind weights (semantic vs. wikilink vs. rarity-tag vs. metadata) — calibrate against the labeled query set in Phase 1.
- [ ] Expansion `decay` constant and the `expansion_score → rank` mapping — both feed RRF only as an ordering, but the constant changes which neighbors surface; calibrate with `k`/`min_cosine`.
- [ ] Graph pass cadence value (`graph_interval_secs`) and whether it is its own interval or chained to the embed tick. (The *race* concern is **resolved** by the audit: the pass takes the same `acquire_lock()` file lock `cortex embed` already uses, so it cannot interleave with an embed write; the missing-embedding skip remains the day-one safety net. Only the cadence number is open.)
- [ ] Should creator/source hub notes live under `system/` or a dedicated `entities/` directory? (Affects `vault::config::ScanConfig`/`WatcherConfig` defaults — must be set together.)
- [ ] Phase 5 predicate vocabulary: open extraction vs. a controlled predicate set (mirroring canonical tags).
- [ ] Contradiction reconciliation policy: recency vs. source-confidence vs. human-in-the-loop flag-only.

## Audit Pass (2026-06-05, intent-layer)

A read-only code audit of the three targeted areas (`vault/src/search`, `cortex/src`, `oracle/src`), routed through the intent-layer maintenance flow, verified the doc's code claims against the source and corrected several. Folded in:

- **`dst` FK vs. dangling wikilinks (correctness fix).** The "`dst` is always a real note path" claim held only for entity edges; `wikilink` edges from `extract_wikilinks` can target not-yet-created notes (danglers are common: `resolve_wikilink -> Option`, many `OutboundLink.exists == false`), which would violate the `dst` FK at insert. Now the graph pass emits wikilink edges for **resolved targets only**, which keeps the cascade contract and is consistent with traversal semantics.
- **Watermark store.** `embedding_config` is feature-gated and has no generic KV API; replaced with an always-present `graph_state` table in `ensure_schema` (or `cortex::state` file fallback).
- **Missing vector-by-path reader.** Semantic-kNN cannot reuse `search_vector` directly (it takes an external query vector; the per-note BLOB decoder is private). Phase 1 adds a `vault::search` neighbor helper.
- **Edges read ownership.** The `expand` method lives in `vault::search::SearchIndex` (oracle is a thin reader and cannot extend the vault crate), mirroring `find_links`.
- **Embed-race Open Question resolved** via the existing `acquire_lock()` file lock; only the cadence value remains open.
- **Precision.** `index_vault` is the sole *deleter* (not sole writer) of `notes`; `find_mention` must also return the matched surface text for piped links; `extract_wikilinks` is private and must be exposed; schema lives in one `ensure_schema()`; filters apply first / limit last; `ScanScope` needs a `Metadata` variant; pragma cite corrected to search.rs:203/215.

A follow-on Architect review (round 3, same date) — given the AGENTS.md provenance above and asked to re-verify each correction against the source — confirmed all of the above and surfaced two further issues, now folded in:

- **Asynchronous watermark race (correctness fix).** Keying the incremental scan solely on `notes.modified_at` strands any note whose embedding lands *after* it was skipped, because `cortex embed` writes `note_embeddings.produced_at` but never bumps `notes.modified_at` (verified `upsert_embeddings_batch`, vector.rs:283). Resolved with **two decoupled triggers**: semantic edges keyed on `note_embeddings.produced_at` (per-row staleness, mirroring `stale_embedding_targets`), wikilink/tag/metadata on `notes.modified_at`. See Data Model + Phase 1 + a watermark regression test.
- **Universal resolve-`dst`-or-skip rule.** The dangling-wikilink fix generalizes: any edge whose `dst` can be absent or go stale (out-of-band-deleted hub note with a stale `entities.hub_path`; unresolved Phase-5 fact object) would abort the whole graph transaction on insert. Stated once as a universal rule — every edge insert resolves `dst` against `notes` and skips if absent — covering wikilink, entity/hub, and fact edges. The CASCADE cleans on delete; resolve-`dst`-or-skip protects inserts. See Data Model + Phase 3 hub-deletion test.

## References
- `notes/memorygraphrag-outperforms-every-rag.md` — three-layer memory, consolidation agents, type/embedding bridging
- `notes/architectural-patterns-for-graph-enhanced-rag-moving-beyond-vector-search-in.md` — three-layer stack, latency budget, stale-edge TTL
- `notes/pinecone-just-demoted-vector-search-heres-the-knowledge-layer.md` — pre-assembled knowledge bundles vs. rediscovery
- `notes/why-the-best-ai-coding-tools-abandoned-rag-and-what-they-use-instead.md` — agentic retrieval; structured vs. unstructured
- `notes/why-your-ai-agents-keep-forgetting-and-how-to-fix-that-vasilije-markovic-cognee.md` — KG + vector memory, per-domain layers
- `notes/ai-memory-framework-memo-skips-llm-retraining.md` — parametric memory (rejected; provenance loss)
- `vault/src/search.rs` — index schema, RRF, `extract_wikilinks`, `recompute_inbound_link_counts`
- `cortex/src/linking.rs`, `cortex/src/config.rs` — current linker targets and config
- `oracle/src/tools.rs`, `oracle/src/server.rs` — `SearchMode`, `find_links`
```
