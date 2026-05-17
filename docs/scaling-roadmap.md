# Scaling Roadmap: Corpus Density, Hybrid Retrieval, Signal Decay

Date: 2026-05-16
Status: anchor doc - references three child design docs to be drafted

## Context

Ingestion runs at ~20 sources/day (YouTube, GitHub, X/Twitter, blogs, articles). At one year that is ~7,000 notes; at three years, ~21,000. The volume itself is not the problem - disk is cheap, ripgrep stays fast. The problems are:

1. **Retrieval** - "I know I saved something about X" with a known query
2. **Discovery** - "what did I save about X that I forgot existed" with no specific query
3. **Synthesis** - "what is the through-line across two years of notes on X"
4. **Signal decay** - every note is equally weighted forever; most are not gold

The existing system is more mature than a casual read of CLAUDE.md suggests. Verified against source on 2026-05-16:

**Already shipped:**

- `vault::search` (`vault/src/search.rs`, 1622 lines, feature-gated, enabled by oracle): SQLite + FTS5 virtual table over `(title, body, tags, summary)`, incremental indexing by mtime, triggers for insert/update/delete, `find_similar` via FTS5-term overlap, `domain_brief`, `tag_search`, `tag_cooccurrence`, inbound/outbound link traversal, orphan detection, duplicate groups, classify stats.
- Oracle MCP exposes 18 tools wrapping the above (`oracle/src/server.rs:193-750`).
- `vault::watcher` (331 lines) wired to oracle reindex.
- Borg intake invariant: synchronous write to `borg-intake.md` before any classification, with DLQ mirror and `replay_of` chaining (`borg/src/intake.rs:73-180`).
- Borg YouTube extractor: yt-dlp metadata, VTT subtitle fetch, audio for Whisper, frame extraction with mpdecimate (`borg/src/youtube.rs:21-485`).
- Borg article extractor: `markitdown` with 30s timeout (`borg/src/extraction.rs:14-66`).
- Cortex post-ingest passes: `autotag`, `quality`, `sweep`, `intel` (daily/weekly digests), `migrate`.

**Real gaps** (where this roadmap focuses):

- ~~FTS5 has a `summary` column but nothing populates it - distilled summaries are not produced at ingest.~~ **Closed by Doc 1 (Phases 1-9).**
- ~~No `Distilled` struct or extractor trait - YouTube and article extraction are bespoke functions.~~ **Closed by Doc 1.**
- ~~No source-type-aware handling for GitHub repos or X/Twitter threads.~~ **Closed by Doc 1 (Phases 4 and 6).**
- No vector embeddings - `find_similar` is FTS5-term overlap, not semantic. (Doc 2 territory.)
- No reciprocal-rank-fusion or hybrid retrieval. (Doc 2 territory.)
- No decay/promotion signal tracking (open counts, search clicks, last-opened). (Doc 3 territory.)
- No cold-note surfacing report. (Doc 3 territory.)

## Architectural Rule: One-Way Data Flow

A single rule governs all three children and resolves the cross-binary coordination question for the SQLite index:

> **Ingestion writers (borg, cortex, user-via-Obsidian) write only to the file system. VaultWatcher is the single indexer that reads the vault and writes SQLite. Anything writing to SQLite outside that path is the bug.**

```
   borg | cortex | user (via Obsidian)
              │
              │  write to filesystem only
              ▼
        Vault markdown file
        ┌─────────────────────────┐
        │ frontmatter:            │
        │   - cortex-* fields     │   structured metadata
        │   - distilled-* fields  │   that can't render as prose
        │ body:                   │
        │   ## Summary            │   rendered prose (parseable)
        │   ## Claims             │   bulleted, with [anchors]
        │   ## Links              │
        └─────────────────────────┘
              │
              │  VaultWatcher sees mtime change
              ▼
        index_vault parses file:
          - body sections   → notes.summary, notes.claims
          - frontmatter     → notes.* governance + cortex-* columns
              │
              ▼
        SQLite (FTS5 + sqlite-vec in Doc 2)
        Readers: oracle MCP, cortex sweep/intel
```

Consequences for the three children:

- **L2 data must live in the vault file** (body or frontmatter). Anything stored only in the index gets clobbered on every reindex because `index_vault` rebuilds derived columns from the vault. Doc 1 enforces this for `Distilled` fields.
- **Signal data (Doc 3) is the only exception.** Search hits, last-accessed timestamps, and inbound-link counts are accumulator state that the user does not edit in Obsidian; they cannot live in the vault file without producing edit feedback loops. Signals live in dedicated index columns that `index_vault` must preserve (not clobber) on reindex. Doc 3 owns this contract.
- **`index_vault`'s write strategy must change.** It cannot `INSERT OR REPLACE` for existing rows - that would clobber signals. For existing rows it `UPDATE`s only vault-derived columns; for new rows it `INSERT`s with signals initialized to zero. Doc 1 lands this change; Docs 2 and 3 depend on it.

Crate separation (borg / cortex / oracle as distinct binaries sharing `vault` as a library) is not the blocker here. The rule prevents resource competition by restricting database writes to one path, regardless of how many binaries exist.

## Three Children

The work splits into three design docs along orthogonal axes. Doc 1 is the foundation - Docs 2 and 3 build on it and are parallel-safe after it lands.

### Doc 1 - Extractor Contract and L2 Distilled Summaries

**Status:** **Implemented (Phases 1-9).** [design/2026-05-16-extractor-contract-and-l2-summaries.md](design/2026-05-16-extractor-contract-and-l2-summaries.md) shipped Phases 1-8; [design/2026-05-16-extractor-contract-l2-phase-9-cleanup.md](design/2026-05-16-extractor-contract-l2-phase-9-cleanup.md) shipped Phase 9 (deferred-item cleanup + non-URL distillers + verbatim preservation).

**Goal:** stop letting the vault grow at raw-byte density. Every ingested source produces a structured distilled artifact alongside the raw note; that distilled summary populates the existing FTS5 `summary` column and becomes the substrate that every downstream tool (search, vector embed, brief synthesis) actually runs against.

**Drafting points:**

- Define `Distilled { summary, claims, tags, links, kind_specific, meta, transcript }` - the single contract every source-type extractor produces. Decide: where it lives (`vault::distilled`? `borg::extractor`?), whether `claims` is `Vec<String>` or richer (e.g. timestamped for YouTube). (**Implemented as shipped:** `vault::distilled::Distilled` with `claims: Vec<Claim>` where `Claim { text, anchor: Option<String> }` for YouTube timestamps; `transcript: Option<String>` added in Phase 9 for verbatim preservation on non-URL kinds whose published note is the only persistent source.)
- Define the `Extractor` trait shape: input is the raw capture (URL + content blob), output is `Distilled`. Decide on async vs sync, error model.
- Ingest-time vs cortex-backfill decision: should `Distilled` be produced synchronously in borg's intake path (added latency, simpler model) or asynchronously by cortex on a follow-up sweep (decoupled, complicates the staged-pipeline gates)? Cross-reference `docs/design/2026-04-19-staged-ingestion-pipeline.md`.
- Wire `Distilled.summary` into the existing `vault::search` `summary` FTS5 column. Confirm the FTS5 triggers fire correctly on update.
- Per-source-type extractor specs:
  - **YouTube** - extend the existing pipeline (`borg/src/youtube.rs`) to emit timestamped claims, not just transcript. Decide Fabric pattern or direct LLM call.
  - **GitHub repo** - net-new extractor. README + topics + stars + last-commit-date + (optional) tree summary. Explicitly **not** a full clone. Decide: are point-in-time metrics (stars, last-commit) frozen at ingest, or does cortex periodically refresh them? Default to frozen; the note's value is "what I learned," not "current popularity."
  - **X/Twitter** - net-new extractor. Thread reconstruction (walk reply chain), then treat as one document.
  - **Article/blog** - keep `markitdown` extraction; add summarization layer producing `Distilled`.
  - **Generic fallback** - what happens when the URL matches no specific extractor.
- Fabric pattern selection per source type. Cost envelope: which model, target tokens per ingestion, daily budget at 20/day.
- **Claims storage** (decided in Doc 1 per the one-way data flow rule):
  - **Body** holds `## Summary`, `## Claims` (bulleted with `[anchor]` markers), and `## Links` as rendered markdown sections. Human-readable in Obsidian, parseable by `index_vault`.
  - **Frontmatter** holds `kind_specific` metadata that does not render cleanly as prose: `cortex-repo-stars`, `cortex-video-duration-seconds`, `cortex-thread-platform`, etc.
  - **Index** derives both via `index_vault` parsing the body and frontmatter on every mtime change.
  - The FTS5 schema (`vault/src/search.rs:121`) gains a `claims` column and per-kind metadata columns; `index_vault` switches from `INSERT OR REPLACE` to `UPDATE`-vault-derived-columns to preserve Doc 3's signal columns.
- Stage placement in the staged ingestion pipeline (`docs/design/2026-04-19-staged-ingestion-pipeline.md`): name the exact stage where LLM distillation runs. Synchronous in borg adds latency but keeps the staged-pipeline gates honest; asynchronous via cortex decouples but means notes exist in a "raw, no summary yet" state - Docs 2 and 3 must handle that state.
- Backfill plan: a cortex subcommand (`cortex summarize --backfill` or similar) that walks existing notes lacking a summary and produces one. Budget, rate-limit, resume semantics.
- Test plan: golden-input fixtures per source type, snapshot the produced `Distilled`.

### Doc 2 - Hybrid Retrieval (FTS5 + Vector + RRF)

**Status:** Phase A Implemented (BLOB column, fastembed adapter, brute-force cosine, RRF fusion, cortex embed loop with transaction discipline, oracle mode dispatch, regression fixture, opt-in latency benchmark). [design/2026-05-16-hybrid-retrieval-fts5-vector-rrf.md](design/2026-05-16-hybrid-retrieval-fts5-vector-rrf.md). Phase B (transcript chunking + max-pool aggregation + distillers amendment) implements next.

**Goal:** semantic discovery on top of the existing keyword retrieval. BM25 (FTS5) wins on proper nouns, exact terms, and rare tokens; vector embeddings win on conceptual overlap. Reciprocal rank fusion combines both with one screen of code.

**Drafting points:**

- Add `sqlite-vec` as an optional vault feature flag (`vec`?) alongside the existing `search` feature, so vector storage co-locates with the SQLite database file already used by `vault::search`.
- Embedding model: local-only via `fastembed-rs`. Candidates: `bge-small-en-v1.5` (~33M params, ~100MB, fast on CPU) or `nomic-embed-text` (better quality, larger). Decide and justify.
- What to embed: the L2 `summary` from Doc 1, **not** the raw note body. Smaller, denser, cheaper to re-embed on model change. The summary is read from the vault file's `## Summary` body section (parsed by `index_vault`, per the one-way data flow rule); embeddings never bypass that path.
- **Transcript embedding for non-URL kinds** (must resolve - new in Phase 9): Doc 1's Phase 9 added a `## Transcript` body section for Image, VoiceNote, Idea, and Vocabulary notes carrying the verbatim Vision+OCR / Groq / user-text input. URL kinds (Article, Repo, Video, Thread) leave `transcript: None` because the origin URL is the recoverable archive. For non-URL kinds the summary is necessarily a lossy collapse of richer source material - a 60-minute meeting transcript collapsed into 2-4 sentences loses the verbatim phrasing the user remembers six months later. Three paths Doc 2 must choose between:
  - **Embed only `summary` for all kinds** (current stance). Simple. Non-URL search-by-meaning misses the verbatim layer.
  - **Embed `summary` for URL kinds; embed `summary + transcript` chunked for non-URL kinds.** Asymmetric per kind. Captures verbatim semantic content. Doubles the embedding row count for non-URL kinds.
  - **Embed `summary` for all kinds; expose `## Transcript` to FTS5 only** (which already happens via the `body` column). Non-URL semantic search stays on summary; verbatim recall lives in keyword search. Cheapest. Asymmetric capability per kind.
  - The choice depends on the chunking decision below: if Doc 2 picks a long-context model (`bge-m3` at 8K tokens), embedding summary+transcript as one row for short non-URL notes is viable; long voicenote transcripts still need chunking.
- **Token limits and chunking** (must resolve): `bge-small-en-v1.5` has a 512-token limit. An L2 summary plus claims (especially timestamped YouTube claims) will exceed that and silently truncate. Two paths:
  - Chunk the `Distilled` into multiple vector rows (summary, claims-batch-1, claims-batch-2…) and aggregate at retrieval (max-pool or mean-pool the scores).
  - Pick a longer-context model: `bge-m3` handles 8K tokens at a larger model footprint.
  - The choice cascades: chunking complicates the embedding-row schema and the RRF math; long-context bloats the BLOBs and slows re-embed passes. Pick one and own the consequences.
- Schema additions to `vault::search`: a vector column (`embedding BLOB`) plus model-version column (`embed_model_version TEXT`) for safe re-embedding. SQLite trigger on `notes` table UPDATE compares old vs new `summary`; on change, sets `embedding = NULL`. The trigger fires from `index_vault`'s UPDATE (the one and only writer path), so the staleness signal is automatic and consistent.
- **Trigger → re-embed bridge** (must resolve): SQLite triggers cannot call Rust. A trigger nulling `embedding` only signals staleness; something must scan for null rows and invoke `fastembed-rs`. Decide where this loop lives:
  - In the cortex daemon as a periodic pass (simplest; ties re-embed cadence to existing scheduler).
  - In oracle's reindex path (couples re-embed to reindex events).
  - As a standalone `cortex embed --backfill` subcommand for manual triggering.
  - Whichever owns it must handle: rate limiting (model is CPU-bound), resume after crash, and the cold-start case (first-ever index build).
- RRF fusion: implement in oracle (where the query lives). Pseudocode shape: query both, normalize each result list to ranks, score `1 / (k + rank)` per list, sum, sort.
- MCP surface: extend `knowledge_search` with a `mode: bm25 | vector | hybrid` parameter, default `hybrid`. Or add a new `semantic_search` tool - decide which.
- Re-embed strategy: when bumping the model, write new column rather than overwrite; rolling re-embed in cortex daemon; cutover when complete.
- Latency budget: target sub-200ms for typical hybrid query on a ~7k-note vault. Measure and document.
- WAL inflation: vector BLOBs co-located in the existing search SQLite file (per `docs/design/2026-04-20-sqlite-ledger-and-views.md`) will balloon the WAL during rolling re-embed passes. Bound batch size; consider `PRAGMA wal_autocheckpoint` tuning or a dedicated vector database file if growth becomes unacceptable.
- Regression tests: 10-20 known queries with expected hit sets, run on every PR that touches retrieval.
- Out of scope for this doc: reranking via a cross-encoder.

### Doc 3 - Decay and Promotion Signals + Cold-Note Review

**Goal:** make the corpus self-curate by surfacing what is not being used. Never auto-delete - always surface for review. The output is a report, not an action.

**Drafting points:**

- Schema additions to `vault::search` for signal tracking. New columns or a sidecar `note_signals` table - decide.
  - `inbound_link_count` - already computable from the index; materialize as a counter for cheap reads.
  - `search_hit_count` - incremented by oracle on every `note_read` / search-result-clicked event.
  - `last_accessed_at` - same trigger, last-touched timestamp.
  - `ingested_at` - already in frontmatter (`ingested:`); copy into the index for queryability.
- **Accumulator-column preservation contract** (load-bearing): signal columns live ONLY in the index - they are not derivable from the vault file. Doc 1's `index_vault` rewrite must `UPDATE` vault-derived columns only and leave signal columns untouched for existing rows; new rows `INSERT` with signals initialized to zero. Without this contract, the very first reindex after Doc 1 ships would zero every accumulated signal. Doc 1 owns the rewrite; Doc 3 owns the schema for the signal columns and the rule that they are accumulator-only.
- Signal source for each: spell out exactly what writes each counter and from where.
  - **Inbound links** - derived from `find_inbound_links`, recomputed on reindex.
  - **Search hits / last accessed** - oracle MCP increments **only on explicit human-intent events**. `note_read` (someone asked to open this specific note) counts. Returning a note in a `knowledge_search` top-10 list does **not** count - that is a lexical match, not a human signal, and counting it creates a positive feedback loop where high-BM25-scoring notes become immortal and the entire decay premise collapses. The bar is "did a human (or an agent acting for one) deliberately look at this note?" not "did the index surface it?"
  - **Obsidian opens** - explicitly **out of scope** unless we ship an Obsidian plugin; file `atime` is unreliable.
- `cortex sweep --cold` subcommand: produce a markdown report under `system/views/cold-notes.md` listing notes older than N days with all signals at zero. Configurable thresholds in cortex config.
- L3 promotion: a `pinned: true` (or `starred: true`) frontmatter flag, queryable via the index. Surfaced separately from cold-note review. Sets a floor - pinned notes never appear in the cold report.
- Output format: rendered as a review checklist, not an action. User decides per row: archive, delete, leave, promote.
- Schedule: tie into the existing `cortex daemon` scheduler. Suggested cadence: weekly cold-note report regeneration.
- Threshold tuning: defaults in code, overridable in cortex config; document what each threshold means.
- Test plan: synthetic vault fixtures with known-stale notes, assert the report surfaces them.

## Dependency Order

The dependency story is softer than a first read suggests. All three docs can start in parallel; Doc 1 gates only the *final wiring* of 2 and 3, not their scaffolding or first implementation pass.

```
Doc 1 (extractors + L2 summaries) ──┐
                                    │
Doc 2 (hybrid retrieval)            ├── final wiring waits on Doc 1
   build sqlite-vec + RRF against   │   (swap embedding target from body → summary)
   existing `body` column today     │
                                    │
Doc 3 (decay signals + cold notes)  ├── final wiring waits on Doc 1
   ship signal tracking + cold      │   (enrich decay model with summary quality)
   report against existing 21k      │
   raw notes today                  │
                                    ▼
                            All three integrate
```

- **Doc 1** owns the `Distilled` contract, the body-rendering format (`## Summary` / `## Claims` / `## Links`), the frontmatter kind_specific fields, and the `index_vault` rewrite that switches from `INSERT OR REPLACE` to `UPDATE`-vault-derived-columns. The `index_vault` rewrite is a contract that Docs 2 and 3 depend on for correctness, not just an internal detail.
- **Doc 2** can implement sqlite-vec, the RRF algorithm, the model loader, and the re-embed loop against the existing `body` column. When Doc 1 lands, the embedding source swaps to `summary` (now populated by `index_vault`'s body-section parser), and the SQLite trigger that nulls `embedding` on summary change ties cleanly into `index_vault`'s single-writer path.
- **Doc 3** is largely Doc-1-independent for the signal-tracking mechanics, but Doc 1's `index_vault` rewrite is a hard prerequisite: without it, the first reindex zeroes every accumulated signal. Doc 3 can draft and prototype against the existing schema today; it cannot ship until Doc 1's preservation contract is in place. The cold-note report and decay scoring then layer on without further architectural friction.

This means: **drafting can fan out immediately**. Implementation can fan out immediately too. Only the final integration steps in Docs 2 and 3 sequence behind Doc 1.

## Out of Scope

Deliberately excluded - revisit only with evidence:

- **L3 curated tier as a formal concept** beyond a simple `pinned` flag. Defer until human promotion patterns actually emerge from Doc 3's review reports.
- **Auto-deletion.** Cold-note review always surfaces for human decision.
- **Cross-vault federation** or multi-vault search.
- **Real-time notification** of new high-signal notes.
- **Cross-encoder reranking.** Defer until hybrid retrieval proves insufficient empirically.
- **Obsidian plugin** for open-tracking. Heavy and side-channel; rely on oracle-mediated access signals instead.

## Cross-References

- `docs/design/2026-04-19-staged-ingestion-pipeline.md` - staged pipeline with gates/replay; Doc 1 must align with the stage model.
- `docs/design/2026-04-20-sqlite-ledger-and-views.md` - the SQLite migration of the ledger; Doc 2 should co-locate vector storage in the same database file.
- `docs/design/2026-03-22-vault-watcher-oracle-reindex.md` - watcher → reindex; Docs 2 and 3 inherit this trigger model.
- `docs/design/2026-03-21-cortex-classify-promote.md` - existing classify pipeline; Doc 1's extractor model should not duplicate.
- `docs/design/2026-04-29-frame-aware-youtube-ingestion.md` - current YouTube pipeline; Doc 1's YouTube extractor extension builds on this.

## Addendum: Flagged Operational Characteristic (Round 3 Architect Review)

The Round 3 architect review of Doc 1 cleared it for implementation but flagged one operational characteristic that is real but bounded, and not a Doc 1 blocker.

**The behavior:** oracle's existing `VaultWatcher` -> `index_vault` path does a full `scan_vault` of ~21k files on every debounced event, even when only one file changed. When `cortex summarize --backfill` runs (sequential atomic rewrites over ~1 hour), each debounce window triggers a 21k-file scan. The mtime skip makes the actual DB work cheap, and rayon parallelizes the scan, but the I/O floor is real.

**Why this is not a Doc 1 blocker:**

- It is pre-existing oracle behavior, not introduced by Doc 1.
- Backfill is a manual operation; if it is slow because of watcher feedback, that is acceptable.
- The debounce naturally coalesces bursts (5s window means ~12 scans/minute max during backfill, not one per file).

**Two mitigation options if it ends up biting:**

- **A.** Add a `cortex backfill --quiet` flag that signals oracle to suspend `VaultWatcher` during the run; cortex triggers one final `oracle reindex` at the end. Small Doc 1 add (one IPC message + one config flag).
- **B.** Accept the operational characteristic; if backfill is observably slow, fix oracle's `index_vault` to take an optional path filter rather than always scanning the whole vault. This is an oracle/vault refactor, not a Doc 1 add.

**Recommendation: B** (accept now, fix in oracle if it bites). The issue is in the oracle daemon and is not Doc-1-shaped work. Adding a `--quiet` flag would couple cortex to oracle's watcher implementation, which the one-way flow rule has spent effort decoupling.
