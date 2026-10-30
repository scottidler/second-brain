# Design Document: Retrieval Quality from the RAG Scour

**Author:** Scott Idler
**Date:** 2026-10-29
**Status:** In Review
**Review Passes Completed:** 5/5 (draft; correctness + clarity rewrite; criteria-execution pass; edge-case pass; voice/lint pass) + panel round 1 folded in

## Summary

A 75-item RAG scour (receipts batch `c1b58314`, ingested 2026-10-28) turned up six changes that fit oracle/cortex retrieval. The biggest is a bug: 90.7% of transcript-chunk vectors are silently truncated at the model's 512-token window, so 24% of all transcript tokens were never embedded. This doc fixes the eval first so every change is measured, then fixes the chunker, adds contextual chunks, kind-aware pooling, a rerank default and a PPR graph scorer, each shipping only on a pre-registered paired-eval rule.

## Problem Statement

### Background

- Scott, 2026-10-29: "find them and summarize. keep an eye for what we might apply to our corpus of obsidian notes". The scour's apply list is this doc's scope; Scott then invoked `/create-design-doc` on it.
- Retrieval today (ripr; live `~/.config/sb/oracle.yml` has no `retrieval:` block, so built-in defaults): vector-only, `bge-small-en-v1.5` (384-d, 512-token window). bm25, graph, HyDE and rerank are built and off.
- Vector kinds per note (`vault::search::vector::EmbeddingKind`):
  - `summary`: 1 per note, `title + capture note + ## Summary` (`cortex::embed::summary_embed_text`)
  - `transcript-chunk`: N per note, a raw sliding window over the transcript (`vault::embedding::chunk_transcript`)
  - `claim`: off since 2026-07-05, it regressed nDCG 0.8795 -> 0.8471 (`cortex::config::EmbedKindsConfig::default`)
- Live counts, ripr `oracle.db`, 2026-10-29: 4922 notes, 4922 summary rows, 19,663 chunk rows over 2176 notes (9.0 per note), 0 claim rows, 308,804 edges. 2101 of the 2176 chunked notes carry claims.
- Prior docs this builds on:
  - `2026-06-06-configurable-retrieval-pipeline.md`: vector-first default (vector 0.876 vs hybrid 0.799); no ANN; no new persistent store for read-path stages
  - `2026-06-06-oracle-eval-baseline.md`: harness, pooled LLM judge, calibration panel (67 labelled pairs)
  - `2026-10-27-cpu-tier-inference-backend.md`: rerank on ripr scored configured+rerank nDCG 0.8482 vs vector 0.6982 (+0.150) at p50 400 ms / p95 721 ms; activation left to Scott; only same-run comparisons are valid
  - `2026-10-14-automated-note-quality-loop-implementation-notes.md`: `--mine-queries` produced 237 queries from clyde sessions, never committed

### Problem

1. **Transcript chunks are truncated.** `chunk_transcript` windows 400 whitespace words (`CHUNK_MAX_TOKENS` in `cortex/src/embed.rs`), about 650 BPE tokens. Both backends set `TruncationParams { max_length: 512 }` on the tokenizer (`vault/src/embedding/candle.rs` `build_inner`, `onnx.rs` `load_tokenizer`), so the overflow is cut with no warning. Measured on ripr, 2026-10-29, live rows against the committed tokenizer fixture:

   | kind | rows | median tokens | p90 | max | over 512 | tokens never embedded |
   |---|---|---|---|---|---|---|
   | transcript-chunk | 19,663 | 650 | 746 | 20,703 | 17,835 (90.7%) | 3,064,045 (24.0%) |
   | summary | 4,922 | 148 | 230 | 38,042 | 27 (0.5%) | 62,499 (7.9%) |

   - Truncation keeps the head. The median window loses ~86 trailing words and the overlap is 50, so ~36 words at every window boundary land in NO vector. These are holes, not shorter chunks.
   - The 20,703-token max shows whitespace words are no token proxy for URLs, code and CJK.
   - `CHUNK_MAX_TOKENS` counts words. The name lies.
   - By type, over-512 chunks: youtube 14,644 of 15,907; article 3,069 of 3,580; note 87 of 97.
2. **Chunks carry no context.** A chunk is a bare transcript window ("he said the second one works better") with no title or topic. The 2026-06-06 doc called contextual retrieval "mostly a no-op here"; that held for summaries only.
3. **Pooling gives long notes extra lottery tickets.** `SearchIndex::search_vector` scores a note by its min distance over ALL its rows; the SELECT never reads `e.kind`. A transcript note gets ~9 chunk vectors plus a summary; a GitHub or entity note gets 1.
4. **Rerank measured +0.150 nDCG and ships off.** `RerankConfig.enabled` carries a field-level `#[serde(default)]` (`false`), so even a partial `rerank:` block resolves off.
5. **The graph method is a 1-2 hop BFS** (`vault::search::graph::SearchIndex::expand_graph`) that lost to hybrid in every eval (-0.22 nDCG at baseline). The scour's best graph result is Personalized PageRank over a relation-free graph (HippoRAG 2405.14831, LinearRAG 2510.10114). Our `edges` table is that graph, built with no LLM extraction.
6. **The eval cannot measure any of this.**
   - 9 committed queries (`config/eval/queries.yml`), 4 with calibration maps.
   - IDCG comes from every judged note in a query's pool, and the pool spans all targets (`metrics::idcg_at_k`, `metrics::pool` over `run.ranked`). Adding a target re-bases every row: rerank grew the pool 187 -> 236 judgments and moved vector 0.7928 -> 0.6982 with identical rankings.
   - The harness opens one DB and one config, so an index or config change cannot be A/B'd in one run.
   - The report prints means only; per-query scores (`metrics::QueryScores`) are never surfaced.
   - An unjudged note scores 0 (`dcg_at_k`, `unwrap_or(0)`), so a judge failure silently lowers a score.
   - The judge sees the summary alone whenever one exists (`calc::prepare_note_text`), never the claims. A note surfaced by a transcript passage is judged on a summary that may not mention it.
7. **A recipe change re-embeds nothing.** Staleness is `e.source_modified_at < n.modified_at` plus the model pin, AND-ed with the `embedding_examined` sentinel (`vector::stale_embedding_targets`). Change the chunker and old rows stay until each note is edited. The only levers are `--drop-kind` (the kind goes dark until the drain finishes) or `--model` (wrong: the model id names the model only, per `vault/AGENTS.md`). The stale query also returns a note once per stale row, before its `LIMIT`.

### Goals

- Every change ships only on a pre-registered rule over a paired eval (below), reported positive or negative.
- Zero transcript-chunk rows over the model window; no unembedded holes between windows.
- Chunks carry title + summary context.
- A recipe change re-embeds the affected kind by construction: no retrieval gap, no model-id change.
- Pooling weights kinds explicitly; rerank and PPR defaults are set by measured deltas.

### Non-Goals

- **Ingest extraction quality**: the scour batch's two wrong-content notes came from borg's `readable` extractor and still passed as `succeeded`. Borg-side, different crate and change cadence; it gets its own doc.
- **talos's `oracle.db`**: Scott ruled 2026-10-29 that no sb service runs on talos. talos's cortex stopped 09:48:49 PDT and its last embedding write was 09:47:58 PDT, so that index is frozen. Re-embeds here run on ripr only.
- **The 27 oversize summaries**: distill defects (max 38,042 tokens). Phase 4 makes them loud; fixing their distill is out of scope.
- **LLM-extracted GraphRAG** (Microsoft GraphRAG, LightRAG, KAG): costly, noisy, drifts. Our deterministic edges cover the use.
- **Semantic chunking**: 2410.13070 found no consistent gain over fixed-size.
- **SPLADE, Matryoshka, RL-trained searchers, an embedding-model swap**: excluded. Revisit only if the grown eval shows a recall ceiling these changes do not move.
- **More fusion or multi-query**: 2603.02153 and our own eval agree it dilutes the vector signal.

## Proposed Solution

### Overview

Measure first, then fix, then tune:

```
operator query set -> paired A/B eval -> recipe versioning -> token chunker -> contextual prefix
                                                           -> kind offsets -> rerank default -> PPR scorer
```

Every change is measured on an **arm**, compared against a baseline in one eval run:

- **index arm**: a `sqlite3 .backup` copy of `oracle.db` under its own absolute `XDG_DATA_HOME`, re-embedded with the new recipe inside `bwrap` with the live data dir bound read-only. `oracle_db_path()` resolves from `XDG_DATA_HOME` only, but a relative value silently falls back to `~/.local/share` (`vault::paths::xdg_data_dir`), so the arm path must be absolute and the read-only bind makes a live write fail loudly.
- **config arm**: a copy of `oracle.yml` with one retrieval setting changed.
- Arms A and B are cut from the same snapshot, so live drift during the experiment does not leak into a delta.

### Decision rule (pre-registered)

- Primary metric: nDCG@10. Secondary, reported only: Recall@10, MRR, p50/p95 latency.
- A change ships iff, on the **confirm** split, the target's paired mean delta has a 95% bootstrap CI (fixed seed, 10,000 resamples) whose lower bound is > 0, and p95 latency stays within the budget named in its phase.
- Queries split deterministically by `fnv(id) % 2` into **tune** and **confirm**. Any phase that picks among settings (Phase 6 offsets, Phase 8 alpha) picks on tune and decides on confirm. Single-setting phases use confirm only.
- MDD (`2.8 * sd / sqrt(n)`, per target, from that target's paired n) is printed as a planning diagnostic, never as the ship rule. n <= 1 or zero variance prints `insufficient` and blocks shipping.
- Any pooled note without a judgment fails the run with a non-zero exit. A judge failure never scores 0.
- Scott approving this doc approves this rule. That is the provenance for Phase 7's rerank flip and Phase 6's claim decision, which the 2026-10-27 doc left to him.

### Architecture

- **Eval (`oracle::eval`)**
  - The operator query set lives in the private vault at `~/repos/scottidler/obsidian/system/eval/oracle-queries.yml`. The public repo keeps its 9-query example. `gh api` reports second-brain `public` and obsidian `private`; the vault is also synced and versioned.
  - `--compare-db <PATH>` and `--compare-config <PATH>` define arm B; whatever is omitted comes from arm A. Every target runs on both arms.
  - **Paired pool, per target**: for each (query, target), the pool is the union of that target's arm-A and arm-B top-K. IDCG comes from that pool only, so adding or removing other targets cannot move a target's delta. The legacy all-target pool stays for the existing single-arm report.
  - Report adds, per target and split: nDCG A, nDCG B, mean delta, 95% CI, n, MDD, verdict (`ship | hold | insufficient`).
  - **Judge text**: `prepare_note_text` becomes `summary + "\n\n" + claims`, capped at the existing `JUDGE_TEXT_MAX_CHARS` (8000) with summary first, claims truncated at a claim boundary; body fallback unchanged when the summary is empty. Fixed per note, independent of target or arm. 2101 of the 2176 chunked notes carry claims, so transcript-driven hits are judged on transcript-derived evidence. The cache keys on content hash, so this re-judges once.
- **Embed recipe (`vault::search`, `cortex::embed`)**
  - Add `note_embeddings.recipe` and `embedding_examined.recipe`, both `INTEGER NOT NULL DEFAULT 1`. Each kind gets a code constant `RECIPE`.
  - Every insert path stamps the kind's `RECIPE`: `upsert_embedding`, `upsert_embeddings_batch`, `swap_kind_chunks`, and the sentinel write.
  - `stale_embedding_targets`: the stale group gains `OR e.recipe < <RECIPE>`; the sentinel clause becomes `x.note_path IS NULL OR x.examined_at < n.modified_at OR x.recipe < <RECIPE>`, so a sentinel written under an old recipe never masks a re-drain. Notes are de-duplicated (`SELECT DISTINCT` on the note key) before the `LIMIT`.
  - Old rows serve until the note's atomic swap; summary rows get the same per-note swap chunks already have.
  - Every chunking parameter is a code constant inside the recipe. There is no config knob that changes embedded text without a recipe bump.
- **Chunker (`vault::embedding`)**
  - `EmbeddingModel` gains `fn max_input_tokens(&self) -> usize` and `fn chunk(&self, prefix: &str, text: &str) -> eyre::Result<Vec<String>>`. Backends use their loaded tokenizer. `MockEmbedder` uses a deterministic whitespace stand-in.
  - `CHUNK_OVERLAP_TOKENS = 64` and `PREFIX_MAX_TOKENS = 64` are recipe constants in vault. `CHUNK_MAX_TOKENS` is deleted.
  - Window capacity = `max_input_tokens - 2 (CLS/SEP) - tokens(prefix)`. A prefix over `PREFIX_MAX_TOKENS` is cut at a token boundary first, so capacity is always >= 446 > overlap.
  - Boundaries come from tokenizer offsets, so each chunk is a verbatim slice. WordPiece can re-tokenize a cut slice longer than its source span, so every final `prefix + slice` string is re-encoded with truncation disabled and shrunk one token at a time until it fits. A tokenizer error is an `Err`, logged with the note path; the note is skipped, never truncated.
  - `group_claims` gets the same token-accurate treatment.
  - A summary text over the window logs one WARN naming the note path and token count.
- **Contextual prefix (`cortex::embed`)**
  - Prefix = `"{title}. {first_sentence(summary)}"`, or `"{title}."` when the summary is empty; chunk text = `prefix + "\n\n" + window`.
  - The chunk arm of `stale_embedding_targets` already selects `n.title`; it adds `n.summary`. `first_sentence` already lives in `vault::detail`.
- **Pooling (`vault::search::vector`)**
  - `search_vector` reads `e.kind` and adds a per-kind distance offset before the min-reduce, configured as `retrieval.methods.vector.kind-offsets: {summary: 0.0, transcript-chunk: 0.0, claim: 0.0}` in `oracle.yml`. All zero reproduces today's ranking exactly.
- **Rerank (`oracle::config`)**
  - On a ship verdict, `RerankConfig` gets a container default and the field default becomes a function returning `true`, so absent, empty and partial `rerank:` blocks all resolve `enabled: true`, and only an explicit `enabled: false` turns it off. The warmup probe still fails open on hosts that cannot meet the budget.
- **PPR (`vault::search::graph`)**
  - `retrieval.methods.graph.algorithm: bfs | ppr`, default `bfs`. `ppr` is forward-push approximate PPR (Andersen, Chung, Lang 2006): teleport `ppr-alpha` (default 0.15), residual `ppr-epsilon` (default 1e-4), work cap `ppr-max-pushes` (default 20,000).
  - Seeds are the fused retrieve-stage list (the RRF output before graph expansion), weighted by reciprocal rank.
  - Adjacency is a request-local map, filled lazily inside one read transaction per query with one prepared statement for the configured `edge-kinds`. Request-local means no cross-process invalidation; one transaction means one consistent snapshot while cortex rewrites edges. Hitting the push cap returns the current estimate and logs a WARN.
  - Graph stays off by default; this phase gives it a fair scorer, not a default.

### Data Model

- `note_embeddings`, `embedding_examined`: add `recipe INTEGER NOT NULL DEFAULT 1` (migration in `vault::search::schema`; existing rows read as 1).
- Recipe constants after this work: summary 1, transcript-chunk 2, claim 2.
- `oracle.yml` `retrieval.methods.vector`: add `kind-offsets`. `retrieval.methods.graph`: add `algorithm`, `ppr-alpha`, `ppr-epsilon`, `ppr-max-pushes`.
- Vault: `system/eval/oracle-queries.yml`, the same schema as `config/eval/queries.yml`.

### API Design

```
sb oracle eval --queries <PATH> [--compare-db <PATH>] [--compare-config <PATH>] [--k 10] ...

  arm A = live DB + live oracle.yml
  arm B = --compare-db (else arm A's DB) + --compare-config (else arm A's oracle.yml)
  report adds, per target and split (tune | confirm):
    target | nDCG A | nDCG B | delta | 95% CI | n | MDD | verdict
  exit != 0 if any pooled note lacks a judgment

sb cortex embed ...   unchanged surface; recipe drift drains through the normal stale query
```

### Implementation Plan

#### Phase 0: Prove an index arm, zero code
**Model:** opus
- Confirm `cortex.yml` sets neither `archive-root` nor `staging-root` (both default under the data home). Observed on ripr 2026-10-29: neither key present.
- `sqlite3 ~/.local/share/sb/oracle/oracle.db ".backup /abs/arm/sb/oracle/oracle.db"`; symlink `/abs/arm/sb/borg` -> `~/.local/share/sb/borg`.
- Inside `bwrap` with `~/.local/share/sb/oracle` bound read-only: `XDG_DATA_HOME=/abs/arm sb cortex embed -v --drop-kind transcript-chunk`, then `XDG_DATA_HOME=/abs/arm sb cortex embed -v --kind transcript-chunk --backfill`.
- Re-mine queries to the scratchpad; record the mined count.
- **Success criteria:**
  - the `-v` log names `/abs/arm/sb/oracle/oracle.db` as the DB path, and the run completes under the read-only bind with no write error
  - arm chunk row count is within 1% of the snapshot's, every arm chunk row has `produced_at` after the spike started, and no note with a pre-spike chunk row lacks one after
  - the doc records the mined query count
- **Observed on ripr 2026-10-29** (sb v0.20.0, HEAD 0155df6; `$ARM` = `/media/saidler/4tb/claude-tmp/claude-1000/-home-saidler-repos-scottidler-second-brain/821cbe7f-1f30-43e6-8fa8-6acb8d69303f/scratchpad/arm0`, kept as the Phase-0 arm):
  - Config: `grep -nE 'archive-root|staging-root' ~/.config/sb/cortex.yml` -> no match, exit 1.
  - Spike start `2026-10-29T18:40:14Z` = epoch `1793299214`. Snapshot: `transcript-chunk|19663|2176` (rows, notes); `summary|4922|4922`.
  - Bind proof: `bwrap --dev-bind / / --ro-bind $L $L -- touch $L/probe-write` (`$L` = `~/.local/share/sb/oracle`) -> `touch: cannot touch '/home/saidler/.local/share/sb/oracle/probe-write': Read-only file system`.
  - Both runs used `bwrap --dev-bind / / --ro-bind $L $L -- env XDG_DATA_HOME=$ARM sb cortex embed -v ...`. Both flags exist as the plan wrote them.
  - Drop: `dropped 19663 embedding rows for kind=transcript-chunk`, exit 0.
  - Backfill: `embed complete: scanned=2720 embedded=19500 skipped_empty=0 failed=0`, exit 0, 0 WARN/ERROR lines. Wall clock 11:40:40 -> 11:56:16 PDT (0.048 s/chunk with the live daemon on the same CPU).
  - DB path, criterion 1: **not met as worded.** `sb cortex embed -v` never logs the oracle DB path (`cortex::embed::run` opens `config.oracle_db_path()` silently). The `-v` log names only arm-derived paths: `writing to: $ARM/sb/cortex.log`, `archive_root=$ARM/sb/borg/sources`, `staging_root=$ARM/sb/borg/stages`. The open file handles of the running process were the substitute proof: `/proc/<pid>/fd` -> `$ARM/sb/oracle/oracle.db`, `oracle.db-wal`, `oracle.db-shm`, and nothing under `~/.local/share/sb/oracle`. Live DB after the run: `transcript-chunk|19663|1793291372` (max `produced_at` is before the spike).
  - Row count: arm 19,500 vs snapshot 19,663, -0.83%. Within 1%: **pass**.
  - Freshness: `min(produced_at)=1793299254`, `sum(produced_at < 1793299214)=0`. **Pass.**
  - Coverage: **fail.** 17 notes (163 rows) had chunk rows in the snapshot and none in the arm; 19,663 - 163 = 19,500 exactly. Root cause: each of the 17 had stale chunk rows (`source_modified_at < notes.modified_at`) and also a current `embedding_examined` sentinel (`examined_at >= modified_at`). The note was re-modified, its transcript source came back empty (12 traces have no `sources/` or `stages/` dir; 5 have a `distilled.yml` with no transcript), so the sentinel was written but the old rows were never deleted. `--drop-kind` deletes rows but not sentinels, so the sentinel masked all 17 from `stale_embedding_targets` and `--backfill` never scanned them. Exactly the 17 snapshot notes with both rows and a current sentinel are missing. The live index serves 163 vectors of a transcript it can no longer produce.
  - Mined queries: `sb oracle eval --mine-queries <scratchpad>/mined-queries.yml` -> `mined 331 distinct queries (441 knowledge_search calls) from 3034 personal-scope sessions`, exit 0. The file stays in the scratchpad.

#### Phase 1: Operator query set
**Model:** sonnet
- `--mine-queries` -> curate (dedupe, drop one-word and session-specific queries) -> `system/eval/oracle-queries.yml` in the vault, keeping `mined-<fnv>` ids so cached judgments carry over.
- Add `calibration: {}` to 20 queries; `--emit-calibration`; **Scott** labels; copy the labels back.
- Document the vault path as the operator set in `oracle/AGENTS.md`.
- **Success criteria:**
  - `sb oracle eval --queries ~/repos/scottidler/obsidian/system/eval/oracle-queries.yml` exits 0 and reports >= 100 queries
  - the calibration panel reports the trust gate passing on > 67 labelled pairs

#### Phase 2: Paired A/B eval
**Model:** opus
- `--compare-db`, `--compare-config`, paired per-target pool, splits, bootstrap CI, MDD, verdict, fail-on-missing-judgment.
- Judge text -> summary + claims; re-run the calibration panel on the Phase-1 labels and record agreement before and after.
- First use, which also gives Phase 7 its baseline: arm B = live `oracle.yml` with `retrieval.rerank.enabled: true`.
- **Success criteria:**
  - `compare_same_arm_zero_delta`: identical arms report delta 0.000 and CI [0.000, 0.000] for every target (MockJudge)
  - `compare_target_set_does_not_rebase`: adding a target leaves every other target's paired delta unchanged, while the legacy single-arm report still re-bases (both asserted)
  - `compare_missing_judgment_fails`: a MockJudge that drops one note makes the run return an error

#### Phase 3: Recipe versioning
**Model:** opus
- Decompose `cortex/src/embed.rs` first (1449 lines against the 1500-line bloat gate).
- Schema migration on both tables, recipe stamping at all four write sites, stale clause + sentinel clause, `DISTINCT` before `LIMIT`, per-note summary swap.
- A re-embed that finds no source text for a kind on a note (empty or missing transcript) deletes that note's existing rows of that kind in the same transaction that writes the sentinel. Phase 0 found 17 notes / 163 chunk rows stuck behind a current sentinel with no source; without this they keep recipe 1 forever and the "exactly one recipe per kind" acceptance criterion cannot pass after the recipe-2 drain.
- `sb cortex embed --drop-kind <K>` also clears kind `K`'s `embedding_examined` sentinels, so a drop plus a backfill cannot silently skip notes (Phase 0's coverage failure).
- `cortex::embed::run` logs the resolved oracle DB path at DEBUG, visible under `-v` (Phase 0's unmet criterion 1).
- **Success criteria:**
  - `recipe_bump_marks_only_that_kind_stale`: bumping one kind's `RECIPE` makes every row of that kind stale, including notes with a current-recipe-less sentinel, and no row of any other kind
  - `recipe_reembed_is_not_stale_next_tick`: after a drain, `stale_embedding_targets` returns nothing for that kind
  - `stale_targets_distinct`: a note with 9 stale chunk rows appears once
  - `empty_source_clears_old_rows`: a re-embed that finds no transcript leaves the note with zero rows of that kind and a current sentinel
  - `drop_kind_clears_that_kinds_sentinels`: after `--drop-kind`, a note sentinelled for that kind is selected by `stale_embedding_targets` again, and other kinds' sentinels stay
  - the stale query on a migrated copy of the live DB is no slower than before the change (Phase 0 saw 18 s per batch boundary)

#### Phase 4: Token-accurate chunker
**Model:** opus
- Trait methods, offset-based windows with re-encode validation, recipe constants, `group_claims` fix, summary WARN; transcript-chunk `RECIPE` -> 2, claim `RECIPE` -> 2.
- Measure on an index arm. Do NOT `otto deploy`: Phase 5 ships both in one drain.
- **Success criteria:**
  - `chunk_fits_model_window` (committed tokenizer fixture, truncation off): every chunk of a 20k-token input, including a no-whitespace URL run and long compound words, encodes to <= 512 tokens with CLS/SEP
  - `chunk_overlap_has_no_gaps`: consecutive chunks share >= 1 source token and together cover every source token
  - paired verdict and CI recorded in the implementation notes

#### Phase 5: Contextual prefix + single rollout
**Model:** sonnet
- Prefix inside the window. Transcript-chunk stays `RECIPE` 2: Phases 4 and 5 ship as one recipe, one drain.
- Index arm with 4+5 against the Phase-4 arm (attributes the prefix), then 4+5 against live (the shipped verdict).
- On a ship verdict: `otto deploy` on ripr; the drain re-embeds.
- **Success criteria:**
  - `chunk_text_starts_with_context`: every produced chunk text starts with `"{title}."`
  - `chunk_fits_model_window` passes with a prefix at `PREFIX_MAX_TOKENS`
  - both paired verdicts recorded

#### Phase 6: Kind offsets
**Model:** sonnet
- `kind-offsets` config; `search_vector` reads kind.
- Config arms on tune: transcript-chunk offset at 0.02, 0.05, 0.1. Best on confirm. Then an index arm with `claim` on plus the chosen offset, confirm only.
- **Success criteria:**
  - `kind_offsets_zero_is_identity`: all-zero offsets reproduce today's ranking exactly on the regression fixture
  - the chosen default and the claim verdict follow the decision rule and are recorded; defaults stay 0.0 and `claim: false` on `hold`

#### Phase 7: Rerank default
**Model:** sonnet
- Config arm: Phase-6 state with rerank on. Confirm split; p95 budget = `latency-budget-ms` (1500) on ripr.
- On `ship`: container default + field default function, per Architecture.
- **Success criteria:**
  - `rerank_default_matches_verdict`: absent, empty, partial and explicit-false `rerank:` blocks deserialize to the recorded verdict, and explicit false always wins
  - paired verdict and p95 recorded

#### Phase 8: PPR scorer
**Model:** opus
- `algorithm: ppr`, request-local adjacency, push cap, config keys. Default stays `bfs`; graph stays off.
- Config arms, both with graph `enabled: true`, `weight: 0.3`, `edge-kinds: [wikilink]`: `ppr` vs `bfs`. Alpha in {0.15, 0.3} on tune; decide on confirm.
- **Success criteria:**
  - `ppr_matches_power_iteration`: on a 6-node fixture, push PPR is within `ppr-epsilon` of a dense power-iteration reference
  - `bfs_scores_unchanged`: legacy BFS scores identical to today on the regression fixture
  - graph p95 with `ppr` <= graph p95 with `bfs` on the same run; paired verdict recorded

## Acceptance Criteria

The token-count command below is the measurement used in this doc, inlined so nothing lands under `docs/`:

```
uv run -q --no-project --with tokenizers python -I -c '
import sqlite3,sys; from tokenizers import Tokenizer
t=Tokenizer.from_file(sys.argv[1]); db=sqlite3.connect(f"file:{sys.argv[2]}?mode=ro",uri=True)
L=[len(e.ids) for e in t.encode_batch([r[0] for r in db.execute("select text from note_embeddings where kind=?",("transcript-chunk",))])]
print("rows",len(L),"over512",sum(l>512 for l in L))' cortex/tests/fixtures/bge-small-en-v1.5-tokenizer.json ~/.local/share/sb/oracle/oracle.db
```

- [ ] After the Phase-5 drain on ripr, the command prints `rows` >= 19,663 and `over512 0`.
  - Observed on main (0155df6, the full measurement script): `transcript-chunk rows 19663 median 650 p90 746 max 20703 over512 17835 90.7% tokens_lost 3064045 24.0% of all tokens`
- [ ] The drain is complete and stamped: `sqlite3 -readonly ~/.local/share/sb/oracle/oracle.db "select kind, recipe, count(*) from note_embeddings group by 1,2"` on ripr shows exactly one recipe per kind, equal to its `RECIPE` constant.
  - Observed on main: `Error: in prepare, no such column: recipe`
- [ ] `sb oracle eval --queries ~/repos/scottidler/obsidian/system/eval/oracle-queries.yml` exits 0 over >= 100 queries.
  - Observed on main: `ls: cannot access '/home/saidler/repos/scottidler/obsidian/system/eval/oracle-queries.yml': No such file or directory`
- [ ] `sb oracle eval --queries <set> --compare-config <arm.yml>` prints a verdict per target and split.
  - Observed on main: `error: unexpected argument '--compare-config' found` (`--compare-db` gives the same error)
- [ ] `rg -l 'kind_offsets|kind-offsets' oracle vault` and `rg -il 'pagerank|\bppr\b' vault/src/search` each list >= 1 file, and every Phase 4-8 implementation note records its verdict and CI (checked by the implementation audit).
  - Observed on main: 0 files match `kind-offsets|kind_offsets` in oracle/vault/cortex; 0 files match `pagerank|ppr` in vault/oracle

## Resolved Decisions

- 2026-10-29: the operator eval set lives in the private vault, never the public repo (repo visibility checked with `gh api`).
- 2026-10-29: judge text is summary + claims, fixed per note, capped at 8000 chars. Arm-dependent text would score one note differently per arm. 2101 of 2176 chunked notes carry claims. (Panel R1 #8.)
- 2026-10-29: paired comparisons use a per-target A∪B pool; the legacy all-target pool stays for the single-arm report. (Panel R1 #6.)
- 2026-10-29: the ship rule is the pre-registered CI rule on a confirm split; MDD is a diagnostic. (Panel R1 #7, staff's position.)
- 2026-10-29: sentinels carry a recipe; every write path stamps one; stale targets are distinct. (Panel R1 #1, #2, #10.)
- 2026-10-29: chunking parameters are recipe constants, not config. A knob that changes embedded text without re-embedding is a trap. (Panel R1 #15.)
- 2026-10-29: arms run inside `bwrap` with the live data dir read-only and an absolute `XDG_DATA_HOME`. (Panel R1 #16.)
- 2026-10-29: Phases 4 and 5 stay separate for attribution but ship as one recipe and one live drain. The architect asked to merge, staff to keep them separate; this satisfies both. (Panel R1 Q4.)
- 2026-10-29: PPR uses a request-local adjacency map inside one read transaction, with a push cap and a p95 criterion. (Panel R1 #3, #4.)
- 2026-10-29: Phase 8's arms both run graph at weight 0.3; at the default 0.0 RRF skips the list. (Panel R1 #12.)
- 2026-10-29: Scott approving this doc approves the decision rule, which is the provenance for flipping rerank and claim defaults that the 2026-10-27 doc left to him. (Panel R1 #17.)
- 2026-10-29: rollback is a forward recipe bump plus a re-drain; an older binary restores no vectors. (Panel R1 #5.)
- 2026-10-29: the eval proceeds at 248 operator queries; the 0.02 MDD target is unreachable from mined queries (needs ~2,060 confirm queries). LLM-synthesized queries rejected: queries written from notes bias toward those notes. Scott chose this after Phase 2 measured confirm MDD 0.0913.
- 2026-10-29: talos is out of scope (Scott's no-sb-on-talos ruling, same day).
- 2026-10-29 (after Phase 0): a no-source re-embed deletes the note's rows of that kind in the sentinel's transaction (Scott, option A over keeping the rows as the transcript's only trace); `--drop-kind` also clears that kind's sentinels; `cortex::embed::run` logs the DB path at DEBUG. All three land in Phase 3.
- 2026-10-29: rerank and kind offsets interact (rerank reorders the top 50 by summary text). Phase 7 runs on the Phase-6 state, so the shipped composition is what gets measured.

## Alternatives Considered

### Lower the word window to ~300, keep counting words
- **Description:** retune `CHUNK_MAX_TOKENS` so the median fits.
- **Pros:** one constant.
- **Cons:** p90 and the 20,703-token max still overflow; the name keeps lying.
- **Why not chosen:** words are no token proxy for URLs, code and CJK.

### `--drop-kind` + backfill instead of recipe versioning
- **Description:** drop the kind and re-embed.
- **Pros:** no schema change.
- **Cons:** the whole kind goes dark until the drain finishes; nothing catches a forgotten drop.
- **Why not chosen:** a recipe change has to re-embed by construction.

### LLM-written chunk context (Anthropic's recipe)
- **Description:** one LLM call per chunk to write its context.
- **Pros:** richer, chunk-specific context.
- **Cons:** ~20k+ LLM calls per re-embed; distill already wrote each note's summary.
- **Why not chosen:** the summary's first sentence is the context at zero cost. Revisit if Phase 5's prefix verdict is `hold`.

### Judge sees the matched chunk
- **Description:** show the judge the passage that retrieved the note.
- **Cons:** the judge text then depends on target and arm.
- **Why not chosen:** fixed per-note text keeps arms comparable; claims carry the transcript evidence.

### MDD as the ship rule
- **Description:** ship when the delta exceeds a run-wide MDD.
- **Cons:** sd differs per target, so a run-wide MDD is undefined; tuning on the same queries overfits.
- **Why not chosen:** a per-target CI on a held-out confirm split is a decision; MDD stays a diagnostic.

### Merge Phases 4 and 5 (architect, R1)
- **Description:** one phase, one recipe bump.
- **Cons:** loses attribution of the prefix's effect.
- **Why not chosen:** one live drain is kept anyway by shipping both under recipe 2 in Phase 5.

### Late-interaction (ColBERT / pylate-rs) reranker
- **Description:** swap the MiniLM cross-encoder for a ColBERT MaxSim reranker.
- **Pros:** strong out-of-domain results in the scour.
- **Cons:** a new model and index shape; the cross-encoder already measured +0.150.
- **Why not chosen:** parked. Revisit if Phase 7 ships rerank and the grown eval shows headroom above it.

## Technical Considerations

### Dependencies
- No new crates. `tokenizers` is already a direct, optional vault dependency; `bwrap` is already used by the cpu-tier Phase 5 method.
- Ship order is inside this repo only: Phase 2 before every measured phase; Phase 3 before 4-5; Phase 5 is the only live re-embed.

### Performance
- Re-embed on ripr: ONNX, 16 threads, ~0.024 s/chunk (measured 2026-10-27). Phase 4 produces more, smaller chunks than today's 19,663. `max-chunks-per-tick` and `max-chunks-per-call` still bound each tick, so the drain spans many daemon ticks while old rows serve.
- The judge-text change re-judges the full pool once (LLM calls through fabric).
- PPR: bounded by `ppr-max-pushes`; Phase 8 asserts p95 against BFS.

### Security
- Mined queries come from personal sessions and stay in the private vault. `--mine-queries` is already fail-closed on scope.
- Arms cannot write the live DB: read-only bind plus an absolute data home.

### Testing Strategy
- Unit tests in sibling `tests.rs` files. Tokenizer tests use the committed fixture (precedent: `cortex/tests/hub_retrieval_contract.rs`). Model-downloading tests go under `#[ignore]`.
- Each named test is shown failing with its change reverted before the phase commits.

### Rollout Plan
- Index changes: arm -> paired verdict -> Phase 5 `otto deploy` on ripr -> drain -> acceptance criteria 1-2 on ripr -> `knowledge_search` exercised from a ripr session.
- Config changes: arm -> paired verdict -> shipped default changes only on `ship`.
- Rollback: a forward recipe bump plus a re-drain. An older binary does not restore old vectors.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Confirm split too small for a CI to clear 0 | High | Med | Phase 2 prints MDD per target. Measured: confirm MDD 0.0913 at n=99 (rerank arm); 0.02 needs ~2,060 confirm queries and mining yields 331. Scott ruled 2026-10-29: proceed at 248 queries; the CI rule stands, so an undetectable change reads `hold` and does not ship |
| An arm writes the live DB | Low | High | read-only bind + absolute path; Phase 0 proves it |
| Judge-text change shifts every absolute number | High | Low | decisions use paired deltas only; calibration re-run in Phase 2 |
| Re-tokenized slice overflows | Med | Med | re-encode with truncation off, shrink to fit; test on compound words |
| Mixed recipes during the drain blur live quality | Med | Low | verdicts come from fully embedded arms, never mid-drain live |
| PPR push storm on a dense node | Med | Low | push cap + request-local map; p95 asserted; off by default |
| An arm's re-embed competes with ripr's live cortex for CPU | High | Low | latency numbers come from eval runs, never during an arm build |
| Curating mined query text changes its `query_hash` | High | Low | only that query re-judges; ids stay `mined-<fnv>` |

## Open Questions

(none)

## References

- Scour batch: receipts `batch_id = c1b58314-818d-468e-b3c9-0860f2663ca4`; notes under vault `notes/`
- Panel round 1 synthesis: `~/.cache/review-panel/runs/WKCkkOlc/synthesis.md`
- HippoRAG 2405.14831; LinearRAG 2510.10114; late chunking 2409.04701; semantic chunking 2410.13070; fusion vs rerank 2603.02153; Anthropic contextual retrieval (vault note `the-best-rag-technique-yet-anthropic-s-contextual-retrieval-explained`)
- Andersen, Chung, Lang, "Local Graph Partitioning using PageRank Vectors", FOCS 2006 (forward push)
- Prior second-brain docs listed under Background
