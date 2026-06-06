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
