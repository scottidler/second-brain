# Relevance-Lift Baseline — `sb oracle eval`

**Date:** 2026-06-06
**Run:** v0.8.58, vault @ ~2151 notes / ~162k edges, judge = fabric default model
**Query set:** `config/eval/queries.yml` (9 grounded queries; 4 hand-labeled)

The first calibrated run of `sb oracle eval`. Committed as a dated baseline so
future runs (after graph/weight changes) show drift against it.

## Results

```
mode                     nDCG@10  Recall@10   MRR     n
bm25                     0.2641     0.1349   0.6667    9/9/9
vector                   0.8802     0.8331   0.8704    9/9/9   <- best
hybrid                   0.8006     0.7595   0.8704    9/9/9
graph                    0.5143     0.4792   0.5741    9/9/9
graph-hybrid             0.5807     0.5159   0.7407    9/9/9
graph-hybrid (no fact)   0.5780     0.5159   0.7407    9/9/9

LIFT graph-hybrid vs hybrid:  nDCG -0.2198   Recall -0.2436   MRR -0.1296
fact-layer ablation: +0.0027 nDCG (1/9 queries touched a fact edge)
calibration (67 hand-labeled pairs): exact 45%  adjacent 84%
  boundary P/R 0.83/0.68  kappa 0.28  -> TRUSTWORTHY
```

## Conclusion

**Graph-augmented retrieval underperforms plain vector/hybrid (-0.22 nDCG vs
hybrid), and the typed fact layer contributes ~0.** Vector is the strongest mode.

The verdict is robust despite an uncalibrated-magnitude judge: the judge's biases
*favored* graph (it over-rated graph's extra claude-code-adjacent content and
under-rated vector's hits), and graph still lost decisively. The judge passed the
trust gate (boundary precision/recall 0.83/0.68); Cohen's kappa is low (0.28) as
expected under pool class imbalance (the reason the gate is precision/recall, not
kappa).

## Why graph hurts (diagnosis from edge composition)

- The graph is **70% shared-tag edges** (blanket-tag co-membership: `llm`,
  `claude`, `agents`) — low signal. At 2-hop expansion + RRF, this noise displaces
  vector's precise hits.
- **Wikilink edges are only 3.6%** — cortex links a 33-concept glossary, so the
  highest-signal edge type is starved.
- Graph expansion also surfaces **entity-hub stubs** (`entities/agents.md`,
  `entities/claude.md`) into results — judged ~0 by both human and LLM.
- **Vector embeddings already capture semantic relatedness**, so for *ranking* the
  graph's extra connections are largely redundant-or-noise.

## Caveats

- 9 queries, single judge run — directional, not statistically significant.
- Magnitudes noisy (judge exact-match 45%, boundary recall 0.68); trust the
  ordering, not the decimals.
- Fact edges erode between backfills (130 -> 36 observed during this session) via
  the deterministic daemon tick's delete-by-src; the weekly fact tick restores
  them. The ~0 fact contribution holds regardless.

## Recommended follow-ups (not yet done)

1. Keep oracle's default retrieval at `vector`/`hybrid`; do not promote
   `graph-hybrid` as a default.
2. If the graph is to help retrieval: cap/down-weight shared-tag edges, exclude
   entity-hub stubs from results, and re-weight the graph list lower in RRF.
3. Densify the high-signal layer: link to the 653 entity hubs by name/alias (not
   just the 33 glossary concepts), and/or promote `entity-proposals.yml`.
4. The graph's clearer value is browsing/reasoning (entity exploration, multi-hop),
   not top-k ranking — measure/position it there.
