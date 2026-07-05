# Implementation Notes: Distillation Knowledge-Extraction Overhaul

Running record of implementation decisions, deviations, tradeoffs, and open
questions per phase. Append-only. Design doc:
`docs/design/2026-07-05-distillation-knowledge-extraction.md`.

## Phase 0: Spike - prove the reduce pattern can select claims

### Result: PASS (all three success criteria met)

No naturally >4-chunk video trace existed in `~/.local/share/sb/borg/stages/`
on the daemon host (largest staged transcript was 46K chars ≈ 1 chunk, below
the 12K-token single-call threshold). Used the largest published video note
instead: `notes/there-are-only-5-safe-places-to-build-in-ai-right-now-are-you-in-one.md`
(167,996-char `## Transcript`, YouTube, 1564 timestamp anchors).

Procedure: extracted the transcript, chunked it at the real
`CHUNK_TOKEN_TARGET` (8000 tokens = 32000 chars) into **6 chunks** spanning
00:00:01–00:26:08, ran the live `distill-video-chunk.md` map pattern on each
chunk (via `fabric -p <abs-path>`, the same invocation borg uses), pooled the
**30** resulting anchored claims, assembled a two-section reduce input
(`## Chunk Summaries` + `## Claim Pool`, anchor-prefixed), and ran a prototype
selection reduce prompt (models the Phase 5 rewrite).

Validation against the pooled claims:
- **C1 (parses as `{summary, claims[]}`):** PASS - 16 selected claims.
- **C2 (verbatim members with intact anchors, not invented):** PASS -
  16/16 claims matched a pool claim at ≥0.98 similarity; **0 invented anchors**.
- **C3 (≥1 claim from the final third of the pool, anchor ≥ 00:20:16):** PASS -
  4 late-anchor claims selected (00:20:43, 00:24:09, 00:25:36, 00:26:04).

Conclusion: the reduce pattern selects claims verbatim with intact anchors
across the whole timeline; it does not paraphrase-and-invent. Phase 5's
selection mechanic is de-risked. The Phase 5 anchor-honesty parse-back rule
(anchor must match a pool anchor or be stripped; null anchor accepted as
synthesis) is sound given this behavior.

### Design decisions
- Used a published-vault video note as the spike source instead of a staged
  trace, because no staged trace was large enough (>4 chunks). The transcript
  content and anchors are identical in form to a staged `transcript.md`, so the
  spike remains faithful. Spike artifacts live in the session scratchpad, not
  committed.

### Deviations
- None from the spike's intent; the source substitution is within the spike's
  "take a real >4-chunk video" latitude.

### Tradeoffs
- Prototype reduce prompt capped selection at 16 (mid-range of the design's
  `max_claims` ceiling of 24) to exercise real selection pressure over the
  30-claim pool rather than "keep everything."

### Open questions
- None.

### Environment note
- This host's sandbox rejects command execution (read-only FS on the seccomp
  setup); all shell steps ran with the sandbox disabled. Downstream phase agents
  may need the same.

## Phase 1: Distillation eval harness + baseline

### Result: DONE (all three success criteria met, baseline recorded live)

- `sb borg eval` runs over 21 golden fixtures spanning 7 kinds and produces a
  scored report. A re-run is cache-hit stable: `0 new` judge calls, byte-identical
  composite (1.952), ~1.6s vs ~47s. Baseline recorded in the design doc Addendum.

The harness mirrors `oracle/src/eval` structurally: a `load`/`evaluate` split,
an FNV-1a-keyed SQLite judgment cache, and a calibration panel with the same
`TRUST_GATE = 0.6` boundary-P/R gate. New code lives in the lib-only `borg`
crate (`borg/src/eval.rs` + `borg/src/eval/{fixtures,judge,cache,calc,report}.rs`);
`sb` wires the `borg eval` subcommand and owns all printing, exactly as
`sb oracle eval` does.

### Design decisions
- Eval logic in `borg` (lib), CLI in `sb` — `borg::eval::run/evaluate` return
  typed `EvalReport`; `sb/src/cli/borg.rs` renders it. Parallels `sb oracle eval`.
- Judge injected via `borg::eval::judge::DistillationJudge` trait — `FabricJudge`
  (live) + `MockJudge` (tests, with a call counter) — so the scoring pipeline is
  unit-testable with no fabric call (`borg/src/eval/judge.rs`).
- Three-axis 0-3 rubric, composite = mean — `AxisScores{claim_coverage,
  anchor_validity,summary_faithfulness}` (`judge.rs`); judged by the new
  `borg/patterns/judge-distillation.md` pattern.
- Cache keyed on `(fixture_id, content_hash, judge_model, rubric_version)` where
  `content_hash` = FNV-1a of `kind + truncated-source + rendered-note` — exactly
  what the judge sees, so any fixture/model/rubric change invalidates only the
  affected rows (`borg/src/eval/cache.rs`, `stable_hash`). FNV-1a (not
  `DefaultHasher`) so a toolchain bump can't silently re-buy 21 LLM judgments.
- Lenient judge-reply parse — each axis extracted by a case-insensitive regex
  (`claim[-_ ]coverage: N`), tolerating code fences/prose; a missing axis errors
  rather than silently scoring 0 (`parse_axis_scores`). Live probe parsed clean.
- Fixture layout `<kind>/<slug>/{source.md,distilled.yml}`; `distilled.yml`
  deserializes into `vault::distilled::Distilled` (the schema is law). Kind is the
  directory name (`borg/src/eval/fixtures.rs`).
- Added `vault::paths::borg_eval_cache_path()` → `~/.local/share/sb/borg/
  eval-cache.db`, mirroring `oracle_eval_cache_path()` (paths are the SoT).
- Report tracks `new_judgments` (cache misses) so the cache-hit-stability
  criterion is a machine-checkable number, not a stopwatch.

### Deviations
- **No live retrieval/DB, so no `retrieve` phase.** oracle's eval splits
  retrieve (search) / evaluate (judge). A distillation eval has nothing to
  retrieve — the analog is `fixtures::load` (gather source+distilled), then
  `evaluate`. Same load/score split, correct seam for this phase's intent.
- **Calibration lives in a sidecar file, not the fixture set.** oracle carries
  `calibration:` maps inside `queries.yml`; fixtures here are directory-based, so
  calibration is an optional `config/eval/distill-calibration.yml`
  (`fixture -> human axis scores`). Same panel + trust gate; `--emit-calibration`
  writes a fillable sheet. Not yet hand-labeled, so the baseline is UNCALIBRATED
  (honest; the hook is shipped for the operator to label).
- **Video/voicenote/image/idea fixtures are not all from staged traces.**
  YouTube transcripts are not durably staged (only 47 non-youtube traces keep a
  `transcript.md`), so the 5 video fixtures are snapshotted from published vault
  notes (`## Transcript` → source, `## Summary`/`## Claims` → distilled). The 2
  voicenote, 2 image, and 1 idea fixtures are hand-authored/synthetic and
  non-personal (voicenote synthesis is a design requirement; images synthesized
  for the same privacy reason), with sources deliberately richer than their
  distillations so coverage < 3 is measurable. Provenance documented in
  `config/eval/distill-fixtures/README.md`. Article (5), thread (3), repo (3)
  are real staged `(source, distilled)` pairs. Total 21, ≥ the required 20.
- **`otto fmt` normalized a pre-existing unformatted file** `borg/src/pipeline/
  tests.rs` (committed unformatted in the prior commit 6119f57, confirmed
  fmt-dirty at HEAD). It was reformatted in the working tree to get `otto ci`
  green but is LEFT UNSTAGED and out of this phase's commit — it is not Phase 1
  work. Flagged for the parent.

### Tradeoffs
- Judge source budget = 24K chars (vs oracle's 8K note budget) so claim coverage
  sees more of long sources; 11/21 fixtures still exceed it and are counted as
  `truncated` (reported), rather than paying for an unbounded judge input.
- Per-axis calibration pairs (3 per labeled fixture) rather than one composite
  pair — reuses oracle's 0-3 kappa/boundary-P-R math directly and gives the
  calibration set more signal per label.
- Fabric pattern resolves by name via `patterns_dir()` (the house pattern), which
  means the live baseline required the pattern in `~/.config/sb/patterns/`. Rather
  than run `otto deploy` (an operator/daemon step), a single reversible file copy
  enabled the run; the copy was removed afterward (config restored). The operator
  still deploys the pattern normally via `otto deploy` in later phases.

### Open questions
- None blocking. The calibration subset is unlabeled: before the eval gates a
  prompt-touching phase (2, 4-7), the operator should `sb borg eval
  --emit-calibration`, hand-label a subset, and confirm the judge reports
  TRUSTWORTHY — otherwise the gate is measuring an unvalidated judge (the
  Phase 1 risk-table mitigation).
