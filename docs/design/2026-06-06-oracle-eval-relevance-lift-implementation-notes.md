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

## Phase 3: Shared mode dispatch

### Design decisions
- **`run_search_mode` is a `pub` method on `OracleMcpServer`** (`server.rs`), not
  a free function, because the graph path reuses the instance helpers
  (`graph_dispatch`, `warn_if_no_embeddings`, `resolve_note_paths`, `err`). The
  eval will construct an `OracleMcpServer` and call this method, guaranteeing it
  measures the identical production dispatch.
- **`knowledge_search` now delegates** to `run_search_mode`; the previous inline
  `match mode {...}` moved verbatim into the method, with `req`-derived params
  (expand_hops clamp, edge_kinds, min_edge_weight) resolved at the call site.

### Deviations
- None — behavior-preserving refactor; all 32 existing oracle tests pass unchanged.

### Tradeoffs
- **Method-on-server vs. free function** — a free function would have required
  refactoring `graph_dispatch` and its helpers off `&self`; the method keeps the
  change small and the shared-path guarantee intact.

### Open questions
- None.

## Phase 4: FabricJudge + judgment cache

### Design decisions
- **`FabricJudge` runs the `judge-relevance` pattern via `vault::fabric::run_pattern`**
  (`judge.rs`) — oracle already depends on vault, so no cortex dependency. Added
  `borg/patterns/judge-relevance.md` and registered it in the bootstrap PATTERNS
  list so `sb bootstrap`/`otto deploy` sync it.
- **`parse_score` takes the first integer token, clamped to `0..=MAX_SCORE`**, and
  errors when none is present (`judge.rs`) — so an unparseable reply becomes an
  uncovered pair (caller WARN+skip), never a silent 0.
- **Stable cache key includes `query_hash` and `content_hash`** (`cache.rs`),
  hashed with `DefaultHasher` (fixed-key, deterministic across runs), plus
  `judge_model` and `RUBRIC_VERSION` ("v1"). Editing the query text, the note
  content, the model, or the rubric invalidates only the affected rows.
- **`truncated` column** persists whether a judgment was made on a truncated body
  (low-confidence), so the report can surface it (finding #1).

### Deviations
- None.

### Tradeoffs
- **`DefaultHasher` over a crypto hash (sha2/blake3)** — deterministic and
  dependency-free; collision risk is negligible for a per-host judgment cache and
  a collision only costs one redundant judge call.

### Open questions
- None.

## Phase 5: Orchestration - calibration + ablation + report

### Design decisions
- **`run` returns `EvalOutcome` (`Report` | `CalibrationSheet`)** (`eval.rs`) so
  `--emit-calibration` writes a fillable sheet and short-circuits before metrics.
- **`EVAL_EXPAND_HOPS = 2`** — graph modes run at 2 hops so 2-hop fact paths
  (`seed -> hub -> fact -> hub`) are actually exercised; the ablation needs this.
- **Lock-then-judge**: Phase A runs every mode + collects pooled note text under
  the DB lock, then the lock is dropped before the slow LLM judging (Phase B), so
  the SearchIndex mutex is never held across fabric calls.
- **Ablation coverage = `graph-hybrid` list != `graph-hybrid (no fact)` list** per
  query; `inconclusive` when zero queries differ (finding #4).
- **Trust gate = mean(boundary precision, recall) >= 0.6** (`report::TRUST_GATE`),
  not Cohen's kappa (finding #3); kappa is still reported.
- **`calibration` is `Option<BTreeMap>`** (`queries.rs`): `None` = not a
  calibration query; `Some({})` = candidate awaiting labels (fillable via
  `--emit-calibration`); `Some(map)` = labeled.

### Deviations
- **Judged text uses `NoteRow.summary` (fallback body), not a separately-parsed
  `## Claims` section** — the design said "summary + claims". The `summary`
  column is the bounded distilled representation already in the index; parsing
  claims out of the body adds risk for little gain. `prepare_note_text` flags a
  truncated *body fallback* as low-confidence (finding #1 intact).
- **Eval graph hops = 2 while the MCP default is 1** — the eval measures the
  graph at its configured max so the fact layer is exercised; production default
  retrieval uses 1 hop.

### Tradeoffs
- **Pure units (metrics, calc, judge parse, cache, report) are unit-tested; the
  full `run` glue is integration-tested via a real `sb oracle eval`** (needs the
  live index + fabric), not a CI unit test.

### Open questions
- Whether to also report graph-hybrid lift at hops=1 (the production default), not
  just hops=2. Deferred to operational tuning.
