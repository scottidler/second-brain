# Implementation Notes: `sb oracle eval` (relevance-lift harness)

Append-only record of how the implementation interprets or diverges from
`docs/design/2026-06-06-oracle-eval-relevance-lift.md`. One section per phase.

## Phase 1: Scaffold CLI + config + judge trait

### Design decisions
- **Module home: `oracle/src/eval.rs` + `oracle/src/eval/`** (`queries.rs`,
  `judge.rs`; metrics/cache/report land in later phases). The judge reuses
  `vault::fabric` (oracle already depends on vault), so no dependency on cortex
  is needed.
- **`EvalQuery.calibration` is an inline `BTreeMap<note_path, score>` on the
  query** (`queries.rs`) rather than a separate file — keeps the labeled subset
  co-located with the query. The two-step emit/fill flow (Phase 5) writes a
  separate sheet that gets merged back into this map.
- **`MockJudge` is a `pub` fixture-driven judge** (`judge.rs`), not test-gated,
  so later-phase pipeline tests in other modules can reuse it; as a `pub` lib
  item it does not trip `deny(dead_code)`.
- **`HIT_THRESHOLD = 2` and `MAX_SCORE = 3` are module consts** (`judge.rs`) so
  the rubric, metrics, and calibration all read the same boundary.

### Deviations
- None.

### Tradeoffs
- **Loader validates eagerly (duplicate ids, score range) at load time** vs.
  deferring to run time — chose eager so a malformed `queries.yml` fails fast
  before any LLM cost.

### Open questions
- None.

## Phase 2: Metrics module

### Design decisions
- **Exponential graded gain `2^rel - 1`, discount `1/log2(rank+1)`** (`metrics.rs`),
  the standard nDCG formulation; a note absent from `judgments` scores `0`.
- **IDCG over the pool's judged scores** (sorted desc, top-k) — `idcg_at_k`. It is
  the max achievable DCG and is invariant to equal-relevance ordering, so nDCG
  needs no tiebreak (ties in the ranked list itself are already deterministic per
  v0.8.56/v0.8.57).
- **`Option` semantics encode query exclusion** (`score_query`): `ndcg = None`
  when IDCG is 0; `recall`/`rr` = `None` when the pool has no relevant note.
  `aggregate` means only the `Some` values and reports per-metric contributing
  counts, so excluded queries never silently count as 0.
- **`pool` returns a sorted, deduped union** so the judged-pair set is
  deterministic.

### Deviations
- None.

### Tradeoffs
- **`reciprocal_rank` returns `Some(0.0)` when relevant notes exist but none are
  in top-k, `None` only when the pool has no relevant note at all** — distinguishes
  "mode missed them" (a real 0) from "query can't discriminate" (excluded).

### Open questions
- None.
