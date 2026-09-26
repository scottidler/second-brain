# Described Tag Classification: Implementation Notes

Design doc: `docs/design/2026-09-25-described-tag-classification.md`

## Phase 0: Measure on production input against a gold set

### Design decisions
- Gold labels (`~/Claude/tags-only-trial/gold.md`): Claude ticked the set and Scott reviewed and accepted it. G2 now records that provenance.
- Wire text (`p0/build.py`): a port of `vault::search::parse_body_summary` plus `body_excerpt`. The title comes from frontmatter `title:`, falling back to the filename.
- Protect-list tags vs. domains: every note carries its domain tag. The `resources` domain is not a tag, so those notes get `entertainment`, `finance` or `books` instead.
- A2-A4 inputs are unclipped (A0 won A0 vs. A1), per "run on whichever of their inputs wins". P1 still ships the 900 clip.
- A4 indicator labels (`p0/arms.py:INDICATORS`): 3 per protect-list tag; the tag's score is the max of its indicators. The indicators are sent without descriptions.
- Screen query embedding: `p0/bge.py`, a numpy forward pass of the cached `BAAI/bge-small-en-v1.5` weights with CLS pooling and L2 norm, as in `vault/src/embedding/candle.rs:380`. It reproduces the stored oracle.db summary vectors at cosine 1.0 (4 of 4 checked). This avoids new repo code and a Rust build.
- The BM25 union takes every phrase hit, not just the top N, so the candidate set can exceed N (for example, `work` matches 1,076 notes).

### Deviations
- Descriptions were drafted from sample-note titles (up to 8 per tag, newest first, the 53 excluded), not from 5-10 full notes. Tags with no samples (`entertainment`, `politics`, `nix`, `synth`) were drafted from the slug alone.
- The drafter had already labeled the gold set, so A2-A4 carry leakage beyond "samples exclude the 53". This is recorded in Phase 0 Results; P5's confirmation on `gold-2` is the control.
- One A4 call's response was truncated in transit (`http.client.IncompleteRead`), so it was never logged, and its cost (about $0.0065) is missing from `calls.jsonl`. `jev.py` now retries transport errors as code 599.
- A6 was first run with L2 1e-2 over 300 iterations, which underfit (F1 0.016). The recorded run is the vectorized refit (L2 1e-4, 2,000 iterations, learning rate 20).

### Tradeoffs
- The instability metric reports both note-level (set differs) and label-level (symmetric difference over union) rates. The doc's "pairwise disagreement rate" is read as note-level for the gates.
- The bootstrap compares mean-over-repeats F1 between arms, rather than pairing single calls, so arms with 3 and 6 repeats compare like for like.
- Cost model: fitted in the doc's form. It is not identifiable with a constant 53-note chunk, so the per-call token and USD figures are given beside it.

### Open questions
- P3-P5: A2 misses "beats baseline" by 0.004 F1 (CI lower bound -0.006), while lifting protect-list recall (work 1/7 -> 4/7). Go on the near-miss, or hold to the rule?
- P6 passes on the baseline's six calls by a small margin (0.195 -> 0.170). Ship `repeats: 3` at 3x the call cost?
