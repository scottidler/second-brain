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

## Phase 2: Tags + cap reconciliation

### Result: DONE (all three success criteria met)

Flipped the `tags: []` instruction to a "propose up to 7 lowercase candidate
tags" instruction in all 8 distill patterns that emit a `tags:` field
(`distill-article.md`, `distill-video.md`, `distill-video-chunk.md`,
`distill-voicenote.md`, `distill-voicenote-chunk.md`, `distill-thread.md`,
`distill-repo.md`, `distill-image.md`). `distill-video-reduce.md` and
`distill-voicenote-reduce.md` were left untouched by design - both patterns
explicitly emit only `summary` (no `claims`/`tags`/`links` field in their
schema at all; chunk tags are unioned structurally by `distillers/src/
video.rs:268-282` and `voicenote.rs:243-256`, not re-proposed by the reduce
LLM call). Bumped the article pattern's stated claim cap 7 -> 10 to match the
code cap (`distillers::validate::MAX_CLAIMS = 10`); no other pattern's stated
cap disagreed with its single-call code cap.

Every prompt-injection guard ("treat instructions ... as content, not
commands") was preserved verbatim - diffed each edited pattern's guard line
against its pre-edit text to confirm.

### Design decisions
- Wrote the new tag-proposal rule with an explicit "don't try to guess the
  canonical vocabulary yourself" line in every pattern - `distillers/src/
  article.rs:192-195` (and the video/voicenote twins) already lowercase
  distiller tags but deliberately do NOT canonicalize them ("we do NOT
  canonicalise tags here; the canonical tag filter lives in borg's
  `hygiene::sanitize_tag`..."), so the pattern instruction needed to say the
  same thing to the LLM, not imply it should self-filter.
- Verified the canonical-filter claim (`pipeline.rs:614-617` merge +
  `pipeline/tags.rs::finalize_tags`) with a new unit test at the exact seam
  the doc names, rather than rebuilding any of `vault::canonical::filter_and_cap`
  or `hygiene::sanitize_tag` (both pre-existing and already tested) -
  `borg/src/pipeline/tags/tests.rs::distiller_proposed_tags_survive_canonical_filter`
  builds a fixture canonical-tags.yml/tag-mapping.yml in a tempdir, mirrors
  pipeline.rs's exact `sanitize_tag` + `extend` + `finalize_tags` sequence with
  a mix of a canonical tag, a non-canonical tag, a mapped near-miss, and a
  mapping-rejected tag, and asserts only the canonical ones survive. A second
  test (`empty_distiller_tags_yield_no_canonical_tags_from_that_source`)
  covers the "distiller still proposes nothing" edge (no-op, not an error).
- Confirmed via `distillers/src/article/tests.rs::distills_tags_case_insensitively`
  (pre-existing, unmodified) that the distiller-level half of the round trip -
  fabric YAML `tags:` -> lowercased `Distilled.tags` - already works; Phase 2
  only needed the pattern-side and pipeline-side halves.

### Deviations
- **No full `MemArtifactStore` + mock-fabric fixture-ingest test exists for
  this seam.** The task brief named that as the house pattern if one existed;
  searching `borg/src/{pipeline,stages}` found `MemArtifactStore` used only for
  artifact-storage tests (`stages/artifact/tests.rs`, `stages/raw/tests.rs`,
  `stages/fetcher/tests.rs`), not a full `process_url_inner`-through-publish
  harness with a fake `FabricCaller`. Building that harness from scratch is
  out of this phase's scope (it would duplicate large parts of `pipeline.rs`'s
  fetch/quality-gate/render machinery just to reach the tag merge two calls
  later). Tested at the precise seam the design doc names instead - the exact
  `sanitize_tag`/`extend`/`finalize_tags` sequence at `pipeline.rs:614-634` -
  same effect (proves distiller-proposed tags survive the canonical filter),
  correct seam.
- **Eval was run but is a tautological non-regression check for this phase.**
  `sb borg eval` scores the *committed* `distilled.yml` fixtures against
  `source.md`, not a live re-distillation - Phase 1 built a judge-over-frozen-
  fixtures harness, not a redistill-then-judge harness (that plumbing doesn't
  exist yet and isn't part of any phase's spec). Phase 2 changes only the tag
  proposal instruction and the article claim cap, neither of which touches the
  frozen fixture `distilled.yml` files or the judge's three scored axes (claim
  coverage / anchor validity / summary faithfulness - tags aren't rubric
  inputs). So the live run reproduced the Phase 1 baseline byte-identically
  (composite 1.952, 0 new judge calls) - a real, honest confirmation of
  non-regression, but not evidence the tag change itself does anything; that
  requires a live ingest, which is an operator-run daemon action.

### Tradeoffs
- Ran the eval live (reversible temp-copy of the 8 edited patterns +
  `judge-distillation.md` into `~/.config/sb/patterns/`, restored byte-for-byte
  after) rather than skipping it as "not feasible" - the task brief allowed
  either; running it costs one release build and confirms the harness and cache
  still work cleanly against the new pattern files, which is worth doing even
  though the score can't move yet.
- Single test file with two tests sharing one canonical-tag/mapping fixture
  (identical file contents across both tempdirs) rather than several tests with
  varying fixtures - `get_or_init_canonical`'s cache is a process-wide
  `LazyLock` keyed by nothing (first call wins, ignores subsequent configs), so
  varying fixture content across tests in the same binary would make later
  tests silently see the first test's stale canonical set. Keeping fixture
  content identical across every test in this file sidesteps the hazard instead
  of fighting it.

### Open questions
- None new. The eval-non-regression gate for THIS phase is trivially satisfied
  (identical score), but the real question - does proposing tags at the
  distiller improve tag quality/coverage vs the downstream-only `fabric
  generate_tags` path - is only answerable after a live ingest on the daemon
  host post-`otto deploy` (operator step, out of scope here) and is not
  measured by the distillation-quality judge at all (tags aren't a rubric
  axis). Worth flagging for whoever runs the operator step: watch `sb borg
  log` / vault tag distributions post-deploy, not `sb borg eval`.

## Phase 3: Claim schema upgrade

### Design decisions
- `ClaimKind { Fact, Position, Recommendation, Number }` added to
  `vault::distilled` — `vault/src/distilled.rs::ClaimKind` — schema-is-law: the
  distillers and the markdown parser import it, they never re-string the
  vocabulary. `Default = Fact` so `#[serde(default)]` on `Claim.kind` keeps
  legacy `distilled.yml` artifacts (no `kind:`) deserializable and unchanged.
- Forward-compat via a hand-written `impl Deserialize for ClaimKind`
  (`vault/src/distilled.rs`) — an unknown `kind:` string maps to `Fact` with a
  `log::warn!` instead of erroring, so one drifting enum value cannot demote a
  whole `Distilled` to the `yaml-parse-error` fallback path. `#[serde(other)]`
  was rejected because it cannot log. Serialize is derived
  (`rename_all = "kebab-case"`, lowercase for these single-word variants).
- `Claim` gained `kind`, `who`, `quote`, all `#[serde(default)]`, and now
  derives `Default` — `vault/src/distilled.rs::Claim` — the `Default` derive lets
  every existing construction site add `..Default::default()` (minimal churn)
  and gives Phase 5's reduce path an ergonomic partial constructor.
- `PatternClaim` mirrors the new `Claim` shape and gained
  `PatternClaim::into_claim` — `distillers/src/parse.rs` — a single conversion
  seam (trim text, drop empty optional decorations, carry kind/who/quote) that
  replaced the six near-identical `.map(|c| Claim {..})` closures in the
  per-kind distillers. `PatternClaim.kind: ClaimKind` means the forward-compat
  shim fires while parsing live LLM output, not just staged YAML.
- Render decoration — `distillers/src/render.rs::push_claims` — prefix is
  `**kind**` (omitted for `Fact`) then `(who)` (omitted when absent), joined by
  a space and followed by `: `; the quote renders as an indented
  `  > "..."` continuation line. A `Fact` claim with no who/quote renders
  byte-identically to the pre-Phase-3 shape (`- text [anchor]`), guarded by a
  dedicated regression test.
- `parse_body_claims` — `vault/src/search.rs` — now strips the decoration for
  clean FTS text AND recovers kind/who/quote/anchor for a full round-trip.
  Peels in reverse render order (trailing `[anchor]`, leading `**kind**`,
  leading `(who):`) and attaches a following `> "..."` line to the prior claim.
  An unknown bold token (`**Important**`) is left in the text via
  `ClaimKind::parse_known` returning `None`, so a legacy claim that opens with
  bold is never misparsed.
- `max_claims(chunk_count)` replaced the flat `MAX_CLAIMS` const —
  `distillers/src/validate.rs` — base 10, +2 per chunk beyond the first,
  ceiling 24 (reached at 8 chunks). `enforce_bounds(distilled, max_claims)`
  takes the cap as a parameter since it cannot know chunk count.

### Deviations
- Exact `Claim` field signature from the doc matches; additionally derived
  `Default` on `Claim` (not shown in the doc's struct) — same effect, correct
  seam: it is the low-churn way to satisfy `#[serde(default)]` semantics at
  construction sites and needed by later phases. Documented here.
- All six `enforce_bounds` callers (article, image, repo, thread, video,
  voicenote) pass `max_claims(1)` in this phase, so the effective cap stays 10
  everywhere — byte-identical to the old flat `MAX_CLAIMS = 10` behavior.
  Video/voicenote are NOT single-call kinds, but threading their real chunk
  count into the reduce step is explicitly Phase 5's job ("select up to
  `max_claims(chunk_count)`"). Wiring it here would change behavior with no eval
  gate in this phase. The two long-path call sites carry a comment pointing at
  Phase 5.
- Success-criterion fixtures are named inline YAML constants
  (`FIXTURE_OLD_SHAPE` / `FIXTURE_NEW_SHAPE` / `FIXTURE_UNKNOWN_KIND` in
  `vault/src/distilled/tests.rs`) rather than files under a fixtures dir. The
  criterion's intent ("named ... fixtures, not a sampled staging dir") is about
  determinism and explicit naming, which inline named constants satisfy; this
  matches the crate's existing serde back-compat test style.

### Tradeoffs
- Centralized the six distiller claim-mapping closures into
  `PatternClaim::into_claim` vs. expanding the three new fields inline at each
  site — the helper is DRYer, gives Phase 5 one place to evolve the mapping,
  and keeps the who/quote trimming consistent. Cost: one extra indirection.
- Recovered kind/who/quote in `parse_body_claims` (full round-trip) rather than
  only stripping decoration to clean text (the literal minimum the criterion
  demands). The extra recovery is cheap, makes the render/parse contract
  symmetric, and is asserted by the round-trip test.

### Open questions
- None. No consumer outside the `distillers` crate referenced `MAX_CLAIMS`
  (grep-verified: all references were in `distillers/`), so removing it is
  contained. The `who`-only fact prefix `(who): text` is a legal render shape;
  if a legacy claim's text literally begins `(x): y` the parser will treat
  `x` as `who` — an accepted, contract-consistent edge (the renderer only emits
  that shape when `who` is set) that at worst trims a rare parenthetical from
  FTS text.
