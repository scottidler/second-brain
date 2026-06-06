# Design Document: `sb oracle eval` — Relevance-Lift Measurement

**Author:** Scott Idler
**Date:** 2026-06-06
**Status:** In Review
**Review Passes Completed:** 5/5

## Summary

A reproducible harness that measures whether oracle's graph-augmented retrieval
(`graph` / `graph-hybrid`) actually improves result relevance over the `hybrid`
baseline. It runs a fixed query set through every search mode, judges relevance
with a pooled, blind LLM-judge (calibrated against hand labels), and reports
nDCG@10 / Recall@10 / MRR per mode plus the lift over `hybrid`.

## Problem Statement

### Background

The graph-augmented-memory feature (shipped v0.8.54-56,
`docs/design/2026-06-05-graph-augmented-memory.md`) added a materialized edge
graph (~162k deterministic edges) and a typed `fact` layer (130 edges), exposed
via oracle's `graph` and `graph-hybrid` search modes. We have verified the
mechanism is **correct** (edges trace to source text; graph mode surfaces
edge-reachable nodes that keyword/vector cannot) and **deterministic** (v0.8.56
fixed the tied-score ordering). What we have NOT established is whether the graph
layer improves **relevance** — whether a user looking for an answer gets better
results with `graph-hybrid` than with `hybrid`.

The graph-augmented-memory design itself deferred this: *"the labeled-query-set
retrieval-lift measurement is an operational benchmark, not a ship-gate."* This
document specifies that benchmark.

### Problem

There is no way to answer "is graph retrieval better, the same, or worse than
hybrid?" with evidence. Any claim of improvement today is unfalsifiable.

### Goals

- Produce a per-mode relevance score (nDCG@10, Recall@10, MRR) and the **lift of
  `graph-hybrid` over `hybrid`**, reproducibly.
- Make the methodology **non-circular**: labels must not be derived from the same
  signals (embeddings, edges) that retrieval uses.
- Make the LLM-judge **trustworthy**: validate it against a small hand-labeled
  set and report the agreement so results carry a confidence caveat.
- Make reruns **cheap and reproducible** via a judgment cache, so the eval can be
  rerun after graph/weight changes.
- **Isolate the fact layer's contribution** via an ablation: `graph-hybrid` with
  vs without typed `fact` edges, so we learn whether the LLM-extracted triples
  actually help retrieval (the headline question behind "a real fact graph").

### Non-Goals

- **Automated query generation at runtime.** Expanding ~10 seed queries to ~30 is
  a one-time authoring step that produces `queries.yml`; it is not a feature of
  the harness.
- **Statistical significance testing.** ~30 queries is too small for meaningful
  p-values; we report raw lift plus a per-query breakdown for eyeballing.
- **Tuning graph weights / `GraphConfig`.** This harness *measures*; tuning is
  downstream work that consumes its output.
- **Absolute recall over the whole vault.** We judge a pool, not all 2143 notes;
  Recall is relative to the pool (standard TREC pooling).

## Proposed Solution

### Overview

`sb oracle eval --queries config/eval/queries.yml` runs each query through all
five modes, pools the union of each mode's top-K, judges every pooled
`(query, note)` pair once (cache-first) with a blind LLM-judge, scores each
mode's ranked list against those judgments, and prints a per-mode metrics table
with lift over `hybrid` and the judge-calibration agreement.

### Architecture

```
queries.yml ─┐
             ▼
   ┌──────────────────┐   for each query, each mode (in-process)
   │  run_search_mode │ ─────────────────────────────────────────┐
   │  (shared w/ MCP) │                                           ▼
   └──────────────────┘                                   ranked lists (5)
                                                                  │
                                                  pool = ∪ top-K  ▼
                                              ┌───────────────────────────┐
                                              │  RelevanceJudge (DI)       │
                                              │   FabricJudge | MockJudge  │  ← blind: query + title/summary only
                                              └───────────────────────────┘
                                                          │ 0..3 scores
                                          cache-first ┌───▼─────────────┐
                                                      │ eval-cache.db    │ keyed (query,note,hash,model,rubric)
                                                      └───┬─────────────┘
                                                          ▼
                                              ┌───────────────────────────┐
                                              │ metrics: nDCG/Recall/MRR   │  per mode, mean across queries
                                              │ + lift vs hybrid           │
                                              │ + calibration κ            │
                                              └───────────────────────────┘
                                                          ▼
                                                   report (table + caveats)
```

### Data Model

`config/eval/queries.yml`:

```yaml
# One-time authored: ~10 hand-written seeds + LLM expansion to ~30.
queries:
  - id: cc-mcp-setup
    query: "how do I configure claude code MCP servers"
    domain: ai            # optional filter passed to search
  - id: ff-pass-install
    query: "youth football pass concepts for install week"
    domain: football
  # ~5 marked for calibration carry hand labels:
  - id: rust-error-handling
    query: "rust eyre vs thiserror when to use which"
    domain: tech
    calibration:          # present only on the ~5 calibration queries
      "notes/....md": 3   # your graded 0..3 labels for pooled notes
      "notes/....md": 1
```

Judgment cache (`~/.local/share/sb/oracle/eval-cache.db`):

```sql
CREATE TABLE eval_judgments (
  query_id        TEXT NOT NULL,
  query_hash      TEXT NOT NULL,   -- hash of the query TEXT; editing the query in
                                   -- queries.yml (same id) must NOT hit a stale
                                   -- judgment made against the old prompt (finding #2)
  note_path       TEXT NOT NULL,
  content_hash    TEXT NOT NULL,   -- hash of the exact note text shown to the judge
  judge_model     TEXT NOT NULL,
  rubric_version  TEXT NOT NULL,   -- bump to invalidate all judgments on rubric change
  score           INTEGER NOT NULL,-- 0..3
  truncated       INTEGER NOT NULL DEFAULT 0, -- 1 = judged on a truncated body (low-confidence, finding #1)
  PRIMARY KEY (query_id, query_hash, note_path, content_hash, judge_model, rubric_version)
);
```

### API Design

```rust
// New trait (DI), mirrors cortex's TripleExtractor / EntityExtractor pattern.
pub trait RelevanceJudge {
    /// Grade how well `note` answers `query`, 0 (irrelevant) .. 3 (perfect).
    /// Receives ONLY query + note title + note text (summary or body excerpt).
    /// Never the mode, score, tags, embeddings, or edges.
    fn judge(&self, query: &str, note_title: &str, note_text: &str) -> Result<u8>;
}

pub struct FabricJudge<'a> { fabric: &'a FabricConfig, pattern: &'a str, timeout_secs: u64 }
// MockJudge in tests returns deterministic scores from a fixture map.

// Refactor: the per-mode match in knowledge_search becomes a reusable fn so the
// eval measures the EXACT code path users hit (no divergent re-implementation).
fn run_search_mode(
    &self, db: &SearchIndex, mode: SearchMode, query: &str,
    domain: Option<&str>, note_type: Option<&str>, status: Option<&str>, limit: u32,
) -> Result<Vec<NoteRow>, McpError>;
```

CLI:

```
sb oracle eval --queries config/eval/queries.yml
               [--k 10]                 # pool/metric depth
               [--judge-model <name>]   # default: cortex llm model
               [--modes bm25 vector hybrid graph graph-hybrid]
               [--rebuild-cache]        # ignore + overwrite cached judgments
               [--report <path>]        # also write the table to a file
```

### Metric definitions (exact)

For a query `q` and a mode's ranked list `L` (top-K), with graded judgment
`rel(n) ∈ {0,1,2,3}` for note `n`:

- **nDCG@K** = DCG/IDCG where `DCG = Σ_{i=1..K} (2^rel(L_i) − 1) / log2(i+1)`, and
  IDCG is the same over the ideal ordering (all pooled-and-judged notes sorted by
  `rel` desc, top K). If `IDCG == 0` (no relevant note in the pool), nDCG = 0 and
  the query is **excluded** from the nDCG mean (it cannot discriminate modes).
- **Recall@K** = `|{n ∈ L : rel(n) ≥ 2}| / |{n ∈ pool : rel(n) ≥ 2}|`. If the
  denominator is 0, the query is excluded from the Recall mean.
- **MRR** = `1 / rank of first n ∈ L with rel(n) ≥ 2`, else 0; mean across queries.
- **Lift** = `metric(graph-hybrid) − metric(hybrid)`, reported per metric.

Relevance threshold for Recall/MRR is `rel ≥ 2` ("good" or "perfect"); marginal
(1) does not count as a hit. This is stated in the rubric the judge is given.
Queries whose pool contains **no** note with `rel ≥ 2` are excluded from the
Recall **and** MRR means (they cannot discriminate modes), with the excluded
count reported.

### Judged text source

**The judge grades the note's distilled representation: title + `## Summary` +
`## Claims`** (the L2 distilled contract, `vault::distilled`), fetched via
`db.get_note(path)`. This is deliberate, and it resolves the truncation hazard
the Architect raised (finding #1):

- The distilled summary/claims are **bounded** (they always fit the judge's
  context — no truncation), **complete** (they are the note's distilled essence,
  not an arbitrary head-of-body excerpt), and **mode-independent**.
- Retrieval returns **notes**, not passages, so note-level "is this note about
  the query" is the correct unit of judgment — and the summary is the faithful
  note-level representation.

**Truncation-bias guard:** for legacy notes lacking a distilled summary, the
judge falls back to title + body rendered to plain text (`[[target|Display]]`
markup flattened to display text) truncated to a fixed token budget — and any
judgment produced from a *truncated* body is tagged `truncated: true` and
reported as **low-confidence** (so a baseline mode that matched dropped tail
content is never silently scored 0 without a flagged caveat). The eval reports
the count of truncated-source judgments; if it is high, the result carries a
"long-document judging unreliable" banner.

`content_hash` is computed over the exact string sent to the judge (summary form
or truncated-body form), so a note edit invalidates only that note's cached
judgment, and the text is identical regardless of which mode surfaced the note —
a judgment is a property of `(query, note)`, never of the mode.

The judge's reply is parsed to a single integer 0–3; out-of-range or
unparseable replies are clamped to range when a leading digit is present, else
the pair is dropped with a WARN and counted against coverage (never silently
scored 0).

### Fact-layer ablation

Beyond the five standard modes, the eval runs one ablation variant:
`graph-hybrid` restricted to deterministic edges only. Comparing full
`graph-hybrid` against this ablation isolates the typed fact layer's marginal
contribution. Reported as its own row with lift vs both `hybrid` and the
ablation.

**`edge_kinds` is an include-list, not an exclude (finding #5).** Verified in
`oracle/src/tools.rs`: `KnowledgeSearchRequest.edge_kinds` is `Option<Vec<String>>`
used as an allow-list by `expand_graph`. So the ablation cannot "exclude fact";
it must pass the explicit include-list of all non-fact kinds. The harness builds
it at runtime via `SELECT DISTINCT kind FROM edges WHERE kind != 'fact'` (so new
deterministic kinds are picked up automatically) rather than hardcoding the six.

**The ablation can be inconclusive, and the eval must say so (finding #4).** With
only ~130 fact edges against ~162k deterministic edges, a 30-query set may never
touch the fact layer — in which case the ablation lift is `0.0` meaning *"not
exercised,"* not *"no value."* Two safeguards:
- **Seed fact-dense queries:** `queries.yml` deliberately includes queries
  targeting entities known to carry fact edges (e.g. claude / anthropic / mcp),
  so the layer is actually exercised.
- **Report ablation coverage:** the eval counts how many queries' pools *changed*
  when fact edges were removed. If coverage is ~0, the row prints
  **"inconclusive — fact layer not exercised (N/30 queries touched a fact edge)"**
  instead of a misleading `0.0` lift.

### Calibration workflow (resolves the label chicken-and-egg)

Hand labels cannot be authored before the runtime pool is known. So calibration
is two-step:

1. `sb oracle eval --emit-calibration <query-ids> --to sheet.yml` runs the
   pool + LLM-judge for the calibration queries and writes a sheet of
   `(query, note, judge_score, human_score: ~)` rows.
2. You fill `human_score`; a normal eval run reads the filled sheet and reports
   judge↔human agreement. Until a sheet is filled, results print with an
   "uncalibrated — judge unvalidated" banner.

**Agreement is reported as a panel, not a single κ (finding #3 — the kappa
paradox).** IR pools are class-imbalanced (most pooled notes are irrelevant), so
chance-agreement inflates and Cohen's κ is artificially suppressed — an accurate
judge can score low κ purely from base rate. The eval therefore reports, and the
trustworthiness gate considers, all of:
- **exact-match %** and **adjacent (±1) %** agreement,
- the judge's **precision and recall at the `rel ≥ 2` boundary** (does the judge
  agree with you on what counts as a *hit*, which is what the metrics actually
  use), and
- **Cohen's κ** (kept for reference, not as the sole gate).

The gate trips low-confidence only if the boundary precision/recall is poor; κ
alone never suppresses an otherwise-agreeing judge.

### Metric reproducibility (no metric-level tiebreak needed)

The metrics consume each mode's ranked list, so reproducibility depends on those
lists being totally ordered. This is guaranteed by stable path-ascending
tiebreakers in every score sort feeding retrieval: `reciprocal_rank_fusion` and
`graph_dispatch` (v0.8.56) and `search_vector` (v0.8.57 — found during the
Architect review of this doc; v0.8.56 had missed the raw vector path). With the
input lists deterministic, nDCG@K and MRR need no metric-level tiebreak. IDCG is
also well-defined without one: it is the maximum achievable DCG, invariant to how
equal-relevance notes are ordered. BM25 (`ORDER BY rank`) is SQLite-deterministic
per database, so it carries no Rust-HashMap-seed nondeterminism.

### Concurrency / safety

The eval is **read-only** on `oracle.db` (it only runs searches, which never take
the embed write-lock) and writes only to its own `eval-cache.db`. It is therefore
safe to run with the `cortex`/`borg` daemons up — unlike the fact backfill, it
does not need the daemon stopped.

### Implementation Plan

#### Phase 1: Scaffold CLI + config + judge trait
**Model:** sonnet
- Add `Eval` to oracle's command enum + `sb oracle eval` in `sb/src/cli/oracle.rs`.
- `config/eval/queries.yml` schema + loader (serde, kebab-case, tilde-expand any paths).
- `RelevanceJudge` trait + `MockJudge` (fixture-driven, for tests).

#### Phase 2: Metrics module
**Model:** opus
- `oracle::eval::metrics` — nDCG@K, Recall@K, MRR, ideal-DCG, pooling/union-dedup,
  relevant-set extraction. Pure functions over `(ranked_list, judgments)`.
- Unit tests with hand-computed fixtures (deterministic, no LLM).

#### Phase 3: Shared mode dispatch
**Model:** sonnet
- Extract the `knowledge_search` per-mode match into `run_search_mode`; call it
  from both the MCP handler and the eval. Existing oracle tests must still pass.

#### Phase 4: FabricJudge + cache
**Model:** sonnet
- `judge-relevance` fabric pattern (rubric: 0..3, blind, threshold semantics).
- `FabricJudge` via `cortex::fabric::run_pattern` plumbing.
- SQLite judgment cache + invalidation keys; `--rebuild-cache`.

#### Phase 5: Calibration + ablation + report
**Model:** opus
- `--emit-calibration` sheet flow; agreement panel (exact %, adjacent %, judge
  precision/recall at `rel ≥ 2`, plus κ for reference); gate on boundary
  precision/recall, not κ alone.
- Fact-layer ablation row via runtime-built non-fact `edge_kinds` include-list;
  report ablation coverage (queries whose pool changed); print "inconclusive" when
  coverage ~0.
- Report renderer: per-mode table, lift vs hybrid + vs ablation, per-query
  breakdown, coverage (judged/total), truncated-source count, excluded-query
  counts, agreement panel, and caveats.
- One-time authoring: expand the 10 seed queries to ~30 → `queries.yml`. Commit
  the first run's report as a dated baseline under `docs/design/`.

#### Phase 6: Tests, docs, ship
**Model:** sonnet
- Full-pipeline test via `MockJudge`; `otto ci`; doc; bump + ship.

## Alternatives Considered

### Alternative 1: Hand-labeled ground truth (no LLM-judge)
- **Description:** User grades every pooled result.
- **Pros:** Gold-standard validity, no judge bias.
- **Cons:** Hundreds of manual judgments per rerun; doesn't scale to iteration.
- **Why not chosen:** Too costly to rerun after each change; LLM-judge calibrated
  against a small hand-labeled subset captures most of the value at a fraction of
  the effort.

### Alternative 2: Structural / proxy labels
- **Description:** Derive "relevant" from vault facts (e.g. wikilink/edge neighbors).
- **Pros:** Zero human effort.
- **Cons:** **Circular** — if "relevant" is defined by edges, graph-hybrid wins by
  construction; the measurement proves nothing.
- **Why not chosen:** Fatal validity flaw.

### Alternative 3: Bash or Python harness
- **Description:** `bin/eval-relevance` (bash) or a `uv` Python script driving
  `sb oracle call`.
- **Pros:** Quick; Python has metric libs.
- **Cons:** Metrics math in awk is fragile and untestable; both re-parse subprocess
  JSON and re-implement the mode dispatch (risk of measuring a different code path
  than users hit); Python adds a non-Rust surface to a self-contained workspace.
- **Why not chosen:** A Rust subcommand reuses oracle internals directly, makes
  metrics unit-testable, and measures the exact production dispatch.

## Technical Considerations

### Dependencies
- Internal: `vault::search` (SearchIndex, modes, NoteRow), `cortex::fabric`
  (LLM plumbing for the judge), oracle's existing dispatch.
- External: none beyond what the workspace already uses (`rusqlite` for the cache).

### Performance
- First (cold-cache) run: ~30 queries × ~15-25 unique pooled notes ≈ 500-750
  sequential judge calls (~3.5s each) ≈ 30-45 min. Bounded and sequential (no
  unbounded fan-out, per project rule). Subsequent runs hit the cache and are
  near-instant unless queries or note content changed.

### Security
- No new external surface; the judge reuses the configured LLM provider. Note text
  is sent to the LLM judge exactly as for classify/distill today.

### Testing Strategy
- Metrics: hand-computed fixtures for nDCG/Recall/MRR/IDCG, pooling dedup, relevant
  set — deterministic, no LLM.
- Pipeline: `MockJudge` drives the whole flow; assert table values from fixtures.
- Calibration: κ computed on a fixture human/judge pair.
- No live-LLM test in CI (judge behind the trait).

### Rollout Plan
- Ship the subcommand; author `queries.yml`; run once to populate the cache and
  produce the first lift report. Rerun after any graph/weight change.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| LLM-judge disagrees with human notion of relevance | Med | High | Agreement panel (exact/adjacent %, boundary precision/recall, κ); gate on boundary precision/recall, not κ alone (kappa-paradox safe) |
| Truncation blinds judge to deep matches, penalizing full-text baselines | Med | High | Judge on bounded distilled summary+claims (no truncation); legacy fallback flags `truncated` judgments low-confidence and reports the count |
| Fact-layer ablation inconclusive (130 facts vs 30 queries) | High | Med | Seed fact-dense queries; report ablation coverage; print "inconclusive" when coverage ~0 |
| Circular labels inflate graph lift | Low | High | Judge is blind to mode/edges/embeddings; pooling is union; relevant-set from judgments only |
| Pool depth (K) caps measurable recall | High | Med | Document Recall as pool-relative (TREC pooling); raise K if needed |
| Small N (~30) → noisy lift | High | Med | Report per-query breakdown; treat as directional, not significant |
| Stale cached judgment after query-text edit | Med | Med | Cache key includes `query_hash`; editing the query invalidates its judgments |
| Judge nondeterminism across cold runs | Med | Med | Judge at temperature 0 where supported; cache freezes first judgment for reproducibility |
| Eval measures a different path than users hit | Low | High | `run_search_mode` shared between MCP handler and eval |

## Resolved Decisions
- **Default `K` = 10** (matches typical result surfacing; `--k` overrides).
- **κ trustworthiness gate = 0.4** (moderate agreement); below → results print a
  low-confidence banner rather than being suppressed.
- **First lift report is committed** as a dated baseline under `docs/design/` so
  future runs show drift; subsequent run-local reports are not committed.

## Open Questions
- [ ] None blocking. (Judge model defaults to the cortex LLM model; revisit only
      if calibration κ is poor.)

## References
- `docs/design/2026-06-05-graph-augmented-memory.md`
- `docs/design/2026-06-05-graph-augmented-memory-implementation-notes.md` (Phase 6)
- `oracle/src/server.rs` (`graph_dispatch`, `knowledge_search`), `oracle/src/tools.rs` (`SearchMode`)
