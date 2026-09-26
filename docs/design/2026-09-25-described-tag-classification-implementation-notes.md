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

## Phase 1: Classifier infrastructure

### Design decisions
- One clip, at the wire: `clip_classifier_text` (`distillers/src/tags.rs`) replaces `body_excerpt`, which is deleted rather than kept as a shim. `TagSources::from_body` (`borg/src/pipeline/tags.rs`) and `cortex::classify::classifier_text` now hold the body whole and leave the clip to the classifiers.
- `FabricClosedVocab::prompt` also goes through `clip_classifier_text`, because `from_body` no longer clips. Without that, the fabric-closed path would send a whole transcript to Fabric, reopening audit M3. The prompt's `TITLE:` line is folded into `TEXT:`, since the clipped wire text already leads with the title.
- The HTTP boundary is a private `Transport` port (`send(body, token) -> (status, body)`), with `UreqTransport` in production. Status handling, parsing, and `ensure_jev` stay in `ClassifierDev::post`, so the fake responder exercises the same path production does.
- Batch sizing (`plan_chunks`, `note_tokens`): a greedy chunk stays under `REQUEST_TOKEN_BUDGET` = 157,708 tokens (the largest passing P0 call) and under `MAX_TEXTS_PER_CALL`. The per-note estimate is the doc's model: 26.9 + 0.43 x wire chars + 24.7 x labels sent + 0.21 x total label chars. On a 59-label bare shard with the 715-char mean P0 wire text, that is about 1,900 tokens per note (P0 measured 100,663 / 53 = 1,899), so a chunk holds about 83 notes. That is under the 159 that 402'd and over the 53 that passed.
- The 402 split only happens on `402` whose body contains `request_spending_limit`. Any other 402, such as the day cap, fails the chunk as it stands and is not retried, which is P2's backstop rule.
- A note that failed one shard is not sent to the next, so a failed note is not paid for twice.
- `CallUsage` rows cover every HTTP call. A refused or failed call is recorded as `ok: false` with 0 tokens and $0, since no response reported a cost for it.
- `sb doctor`: new `classifier_settings_finding` prints one info line each for borg and cortex. The live probe (`classifier_probe_findings`) still runs on borg's config, as before.

### Deviations
- `score_batch`'s signature matches the doc. `finish` now takes the merged `&HashMap<String, f32>` for one note instead of the per-shard slice, and `merge_shard_scores` does the merge inside `score_batch`. Same effect, correct seam.
- `ensure_jev` still fails the whole call when any result is non-Jev, rather than failing per note. The notes in that chunk then get `Err` (per note, as the doc requires), but one non-Jev answer fails its chunk-mates too. I kept this fail-closed rule unchanged because the doc does not ask for a change.
- The "later repeat failed" half of success criterion 3 cannot be tested yet: repeats are P6. The shard half is tested.

### Tradeoffs
- The token budget is the measured pass (157,708) rather than an extrapolation toward the $0.01 cap, which would be about 238k tokens at $0.042/M. The measured number costs a few more calls, but it never plans a chunk P0 did not see pass, and the 402 split still covers drift.
- `Transport` is crate-private. P2/P8 tests in cortex may need a fake across crates. Widening it then is one `pub`; exposing it now would be work for a later phase.

### Open questions
- None.
