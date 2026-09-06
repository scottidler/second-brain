# Design Document: video distill token budget

**Author:** Scott Idler (via agent)
**Date:** 2026-08-30
**Status:** Draft
**Review Passes Completed:** 5/5 + review-panel rounds 1-2 (findings folded in 2026-08-30)
**Ready to build:** NO - one entry in Pending Decisions

## Summary

Fabric hardcodes `max_tokens = 4096` on its Anthropic client and offers no flag
to change it. `claude-sonnet-5` spends part of that same budget on thinking
tokens, and its thinking is UNBOUNDED: measured against the real pattern, one
run in five consumed 4,095 of the 4,096 tokens thinking and emitted a single
token of text. The `distill-video` single-call path needs ~2,860 output tokens
of YAML, so the call runs out of budget and the YAML is cut mid-string. Fabric
exits 0 anyway. Three notes published broken on 2026-08-30, and the map-reduce
path's reduce call has hit the same truncation once (`ht-99d3c5`, 2026-08-08).

This splits each fat pattern call into three narrow ones along field boundaries,
and closes the silent-truncation hole where a partially-emitted document still
parses clean. The split alone is NOT sufficient - Phase 0 measured a narrow call
truncating on unbounded thinking - so the short-path enumeration call needs a
second guard. WHICH guard is the one open decision in this doc; see Pending
Decisions. The reduce path needs none: measured N=8, worst total 774 of 4,096.

## Problem Statement

### Background

- Borg distills YouTube transcripts through fabric patterns
  (`~/.config/sb/patterns/`, sourced from `borg/patterns/`).
- Two paths, chosen by transcript size (`distillers/src/video.rs:117-123`):
  - `<= 12,000` approx tokens: `distill_short` makes ONE call to
    `distill-video`, which produces every field (`video.rs:154-180`).
  - `> 12,000`: `distill_long` chunks at ~8,000 tokens, runs
    `distill-video-chunk` in parallel at concurrency 4 (`video.rs:206-228`),
    then ONE `distill-video-reduce` call (`video.rs:340`).
- Every call goes through `vault::fabric::run_pattern`, which shells out to the
  `fabric` binary and checks only the exit status (`vault/src/fabric.rs:127`).
- Parsed output lands in `PatternYaml` / `ReduceYaml` (`distillers/src/parse.rs:905`,
  `:597`). EVERY field is `#[serde(default)] Option<...>`.

### Problem

Four findings, one root cause and three consequences.

**1. The output budget is smaller than the job, and thinking is unbounded.**

`fabric` sets `ret.maxTokens = 4096` in its Anthropic plugin
(`internal/plugins/ai/anthropic/anthropic.go:54`, v1.4.470). It honors
`opts.MaxTokens` when set (`:238-240`), but no CLI flag populates it, in v1.4.470
or on `main`. So 4,096 is the ceiling for every `fabric -p` call.

`claude-sonnet-5` returns a `thinking` block by default and those tokens count
against `max_tokens`. Measured against the real `distill-video.md` pattern and a
real transcript (`DWWrLlM3gwQ`, 26,700 chars):

| max_tokens | stop_reason | output | thinking | text | result |
|---|---|---|---|---|---|
| 4096 | `max_tokens` | 4096 | 1562 | 2534 | cut mid-string |
| 4096 (thinking disabled) | `end_turn` | 1670 | 0 | 1670 | complete |
| 16384 | `end_turn` | 5679 | 2819 | 2860 | complete, 19 items, 10 claims |

The text alone wants 2,860 tokens. That leaves ~1,236 for thinking against a
measured spend of 1,562 to 2,819.

The sharper finding, measured in Phase 0 AFTER the review panel challenged the
budget table: **thinking has no ceiling below `max_tokens`.** On the narrow
`-enumeration` prompt (which emits ~1,280 tokens of text, well inside budget),
one run in five still returned `stop_reason: max_tokens` having spent 4,095
tokens thinking and 1 token on text. Narrowing the job does not bound the
deliberation. See `2026-08-30-narrow-prompt-spike-measurements.md`.

**2. Fabric reports success on a truncated response.**

A `max_tokens` stop is not an error. Fabric prints the partial text and exits 0.
`run_pattern` only inspects `status.success()`, so the truncated document flows
into the parser as if it were complete.

**3. A truncated document that still parses is invisible.**

Because every `PatternYaml` field is an optional serde default, a cut that lands
between fields yields a VALID document with fields missing. Observed on trace
`cl-1dc7db93`: `claims=0 tags=0 links=0 fallback=none`. The note published clean
with 9 of ~20 enumerated items, no claims, no error anywhere.

**4. Thinking spend is concentrated in exactly one call, not spread evenly.**

Measured per narrow call at model-default thinking on the same transcript:
`-enumeration` spent 1,415-4,095, `-summary` spent 0-38, `-ideas` spent 343-787.
The enumeration gate ("procedural asides are NOT an enumeration") is what burns
the budget; the other two calls barely deliberate at all. This is what makes a
targeted fix possible instead of a global one.

### Evidence

- Three notes ingested 2026-08-30 published with the `[yaml-parse-error]`
  fallback body: traces `ht-1b3f7659`, `ht-73eeb2e3`, `ht-4636d52d`. All three
  logged `fallback=yaml-parse-error`, all three `mark_succeeded degraded=true`.
- Staged `raw-output` for all three ends mid-quoted-scalar. Truncation points
  differ per run: 2,021 / 3,040 / 3,149 / 4,451 / 6,528 bytes across five
  attempts on the same three videos.
- 11 replays of `DWWrLlM3gwQ` produced `yaml-parse-error`, `fabric-error`, and
  `fallback=none, claims=0` in rotation. It never produced a full distillation.
- The long path fails the SAME way, less often. Trace `ht-99d3c5`, 2026-08-08
  01:13:11 (`borg.log.4`):
  `reduce yaml parse failed: found unexpected end of stream at line 83 column
  163, while scanning a quoted scalar at line 83 column 11`. That is the
  truncation signature. It published `fallback=reduce-selection-failed` with 24
  chronological claims and NO tldr, enumeration, or key_ideas, because the
  reduce fallback arms leave those empty (`video.rs:337-339`).
- Rate: exactly one reduce truncation across all six log rotations, which cover
  2026-08-04 onward. Older evidence is gone, so this is a floor, not a total.
  13 long-path videos ran on 2026-08-30 alone, all clean. The chunk step has
  never failed to parse in the retained window.
- Full Phase 0 spike numbers, including the run that truncated a narrow prompt:
  `docs/design/2026-08-30-narrow-prompt-spike-measurements.md`.

### Goals

- A video distillation never silently loses fields to an output cap.
- Every pattern call fits inside 4,096 tokens with the thinking spend BOUNDED,
  not merely hoped to be small.
- A truncated or field-starved parse degrades LOUDLY: named `fallback-reason`,
  never a clean-looking note.
- No fork of fabric, no fabric REST service.

### Non-Goals

- Raising fabric's `max_tokens`. Rejected below.
- Changing `distill-video-chunk`. Re-measured 2026-08-30 by running the real
  pattern over all 5 chunks of the long-path control (`ht-c096e5e2`, 134,273
  chars): outputs 890 / 1,923 / 1,165 / 781 / 1,320, thinking 0 / 823 / 12 / 0 /
  597, every one `end_turn`. Worst case 1,923 of 4,096, so ~2,170 tokens of
  headroom. It has never failed to parse.
- Applying this to `article` | `thread` | `session` | `voicenote`. MEASURED, not
  assumed: all four clear 4,096 at their maximum single-call input size. See
  Phase 0. `distill-session` DID log `partial-chunk-failure` twice on 2026-08-15
  (`hv-f5fbc4`, `hv-fe6176`), but that root cause was diagnosed as duplicate-key
  drift in `2026-07-24-harvest-distill-parsing-robustness.md`, not truncation.
  Revisit condition for `article` specifically, since it is the structural twin
  (the only sibling carrying both `enumeration` and `key_ideas`): a listicle
  article with more than ~20 enumerated items, or any `article` trace logging
  `yaml-parse-error`.
- Retrying a truncated call. Rejected below FOR THE FAT PATTERN. It is live again
  as option A of the one Pending Decision, because the narrow prompt's
  per-attempt success rate (measured 8/12) is nothing like the fat pattern's
  0-for-11.
- Fixing the enumeration GATE's nondeterminism. Phase 0 measured the same
  borderline transcript yielding either 19 items or `enumeration: null` across
  repeated runs at every thinking setting. That is pre-existing behavior, it is
  not caused or worsened by this change, and `null` is a legitimate answer that
  produces no fallback. Recorded under Parked so it is not rediscovered.

## Proposed Solution

### Overview

Two levers. Lever 1 is settled; lever 2's form is the one Pending Decision.

**Lever 1: split each fat pattern call into narrow ones along field
boundaries**, arranged in two waves. Enumeration is the producer. Everything
downstream is a consumer. This bounds the TEXT each call must emit. SETTLED.

**Lever 2: a second guard on the SHORT-path `-enumeration` call.** Lever 1 alone
is measurably insufficient: a narrow call still spent the entire 4,096 budget
thinking. The guard is either bounded retry on a loud truncation (option A) or
`--thinking=off` plus an absent-enumeration signal (option B). PENDING - see
Pending Decisions. The reduce path takes NO lever 2: measured N=8 at both
settings, 8/8 detection, worst total 774 of 4,096.

```
short path (<= 12K tokens)              long path (> 12K tokens)
input: transcript + capture note        input: chunk summaries | claim pool
                                               | enumeration candidates

                                        N x distill-video-chunk (parallel, 4)
                                                     |
wave 1  distill-video-enumeration       wave 1  distill-video-reduce-enumeration
        [lever 2: A or B]                       [no lever needed]
             |                                       |
        +----+----+                             +----+----+
wave 2  |         |                     wave 2  |         |
   -summary   -ideas                       -summary   -ideas
   (summary,  (key_ideas,                  (summary,  (key_ideas,
    tldr,      claims)                      tldr)      claims)
    tags,
    links)
```

Wave 2 runs both calls in parallel with thinking left at model default. Wave 2's
ideas call receives wave 1's item names so the existing `key_ideas` MUST NOT
repeat rule still holds; Phase 0 verified 0 repeats across the split.

### Lever 2 on the SHORT path is an UNRESOLVED DECISION

An earlier revision of this section claimed thinking-off was safe, on the
strength of a 4-run test against an unambiguous listicle
(`top-10-claude-code-skills-plugins-clis-april-2026`, ground truth 10 items):

| setting | runs | items detected | worst output | thinking |
|---|---|---|---|---|
| default | 4 | 10, 10, 10, 10 | 966 | 163-245 |
| `off` | 4 | 10, 10, 10, 10 | 684 | 0 |

That table is real and still holds: on a CLEAR enumeration, thinking-off costs
nothing and the model only spends 163-245 thinking tokens anyway.

**Review-panel round 2 challenged the generalization, and it was right.**
Re-measured at N=12 on the AMBIGUOUS corpus (`DWWrLlM3gwQ`, `declared_count:
null`):

| setting | items detected | `enumeration: null` (parses clean, no fallback) | truncated (loud) | max thinking |
|---|---|---|---|---|
| default | 7/12 | 3/12 | 4/12 | 4,095 |
| `off` | 4/12 | 8/12 | 0/12 | 0 |

Thinking-off is WORSE on both axes for the case it was added to fix. It detects
less, and - the decisive part - it converts a LOUD failure into a SILENT one. At
default the bad run truncates, so the YAML is cut, `EnumerationYaml` fails to
parse, and `wave-enumeration-failed` is recorded. At `off` the bad run is a clean
15-token `enumeration: null` at `end_turn` that parses perfectly, because
`enumeration` is deliberately optional, fires no fallback, and cannot even trip
`mark_enumeration_shortfall` (there is no `declared_count` to fall short of).
That collides head-on with this doc's first stated Goal.

This is NOT a claim that `null` is the wrong answer for this video: the gate is
genuinely ambiguous and `null` may be correct. The finding is narrower and does
not depend on ground truth - **the setting this design picks moves the silent
rate from 3/12 to 8/12, and nothing observes the difference.**

The short-path lever is therefore an open decision, recorded under Pending
Decisions below. The reduce path is unaffected: measured N=8 at both settings,
8/8 detection, worst total 774 of 4,096, so it needs no lever either way.

### Field ownership

| field | short path pattern | reduce path pattern |
|---|---|---|
| `enumeration` | `distill-video-enumeration` | `distill-video-reduce-enumeration` |
| `summary`, `tldr` | `distill-video-summary` | `distill-video-reduce-summary` |
| `tags`, `links` | `distill-video-summary` | (chunks, merged structurally) |
| `key_ideas`, `claims` | `distill-video-ideas` | `distill-video-reduce-ideas` |

### Measured budget

Worst observed TOTAL output (text + thinking) per call against 4,096, on the
34,092-char `DWWrLlM3gwQ` transcript. This is the honest column: the earlier
draft compared text-only against 4,096, which hid the very tokens that cause the
bug.

| call | thinking | worst text | worst thinking | worst TOTAL | spare |
|---|---|---|---|---|---|
| `-enumeration` | `off` | 1,272 | 0 | **1,272** | 2,824 |
| `-summary` | default | 472 | 38 | **510** | 3,586 |
| `-ideas` | default | 1,266 | 787 | **1,980** | 2,116 |

For contrast, `-enumeration` at DEFAULT thinking across 5 runs: totals of 2,695 /
2,883 / 3,328 / 3,556 / **4,096** - the last being a `stop_reason: max_tokens`
truncation. That row is why lever 2 exists.

Dense-enumeration extrapolation, for the supported cap: 19 items cost 1,209-1,347
text tokens, so ~65 tokens/item net of scaffolding. At `MAX_ENUMERATION_ITEMS =
30` that is ~1,950 text tokens; with thinking `off` the total stays ~1,950, or
~2,100 spare. This is EXTRAPOLATION from measured per-item cost, not a measured
30-item run - no 25+ item video exists in the corpus. It is recorded as an open
question rather than claimed as proven.

### Architecture

- `distill_short` becomes a two-wave orchestration. Wave 2 uses
  `futures::join!` on the two calls. Both waves take the same transcript;
  wave 2's ideas call additionally takes the wave-1 item names.
- `distill_long` keeps its map step untouched and replaces the single
  `PATTERN_REDUCE` call with the same two-wave shape over the existing
  `build_reduce_input` sections.
- Each wave call parses into a NARROW serde struct, not `PatternYaml`. A struct
  whose fields are all required for that call is how a truncated-but-valid
  document becomes a parse error instead of silence. This remains load-bearing:
  thinking is bounded on wave 1 but left at default on wave 2, so a wave-2
  truncation is still possible and must be caught loudly rather than prevented.
- Results merge into one `Distilled` exactly as today.

### Data Model

Three new narrow parse structs in `distillers/src/parse.rs`, mirroring the
existing naming:

```rust
pub struct EnumerationYaml { pub enumeration: Option<PatternEnumeration> }
pub struct SummaryYaml { pub summary: String, pub tldr: Option<String>,
                         pub tags: Option<Vec<String>>, pub links: Option<Vec<PatternLink>> }
pub struct IdeasYaml { pub key_ideas: Vec<String>, pub claims: Vec<PatternClaim> }
```

`summary`, `key_ideas`, and `claims` are NOT `Option`. A call that returns
without them fails the parse loudly, which is the point.

`enumeration` stays optional: `null` is a legitimate answer.

The existing `missing-summary` reason (`video.rs:458`) becomes unreachable on
the wave path: an absent `summary` is now a serde error, not an empty string.
The reason string stays in the vocabulary (`image` | `repo` | `article` |
`thread` | `voicenote` | `session` all still emit it), but the video assertion
at `distillers/src/video/tests.rs:701` must be retargeted in Phase 1 rather than
deleted, so the empty-summary path is still proven to degrade.

One new field on `ValidationMeta` (`vault/src/distilled.rs`):

```rust
/// Every wave that failed, in wave order. DISTINCT from `fallback_reason`,
/// which holds only the single most-destructive outcome: a compound failure
/// (enumeration AND ideas) must stay observable, and one string cannot carry
/// two. Mirrors the `enumeration_shortfall` precedent, which is a separate
/// field for exactly this reason.
#[serde(default)]
pub wave_failures: Vec<String>,
```

This is the review panel's C4, and it dissolves the precedence question rather
than answering it: `fallback_reason` keeps its single-value contract for the
operator's headline, `wave_failures` keeps the full set for forensics. Shape
mirrors the existing `bounds_truncations: Vec<String>` on the same struct.

### API Design

New constants in `video.rs`, replacing `PATTERN_SHORT` and `PATTERN_REDUCE`:

```rust
const PATTERN_ENUMERATION: &str = "distill-video-enumeration";
const PATTERN_SUMMARY: &str = "distill-video-summary";
const PATTERN_IDEAS: &str = "distill-video-ideas";
const PATTERN_REDUCE_ENUMERATION: &str = "distill-video-reduce-enumeration";
const PATTERN_REDUCE_SUMMARY: &str = "distill-video-reduce-summary";
const PATTERN_REDUCE_IDEAS: &str = "distill-video-reduce-ideas";
```

`distill-video.md` and `distill-video-reduce.md` are deleted.
`distill-video-chunk.md` is unchanged.

**Threading the thinking flag.** `vault::fabric::run_pattern` has 26 callers
across five crates, so its signature does NOT change. Instead:

- Add `pub enum Thinking { Default, Off }` and
  `run_pattern_with_thinking(..., thinking: Thinking)` to `vault/src/fabric.rs`.
  `run_pattern` delegates with `Thinking::Default`, so all 26 existing callers
  compile untouched.
- `build_fabric_command` appends `--thinking=off` when `Thinking::Off`.
- Add a `thinking: Thinking` field to `distillers::fabric::FabricRequest`
  (defaulting to `Default`), threaded through `FabricShell::call`. `FakeFabric`
  records it so a test can assert wave 1 asked for `Off`.

`--thinking=off` is the ONLY reachable value: Phase 0 measured `low`, `medium`,
`high`, and numeric budgets all returning HTTP 400 through fabric v1.4.470,
because fabric emits the legacy `thinking.type.enabled` shape that
`claude-sonnet-5` no longer accepts. The model wants `thinking.type.adaptive`
plus `output_config.effort`. A bounded-effort setting would be strictly better
than `off` and is recorded under Parked pending a fabric fix.

New fallback reasons, joining the existing vocabulary (`empty-transcript`,
`fabric-error`, `missing-summary`, `partial-chunk-failure`,
`reduce-selection-failed`, `yaml-parse-error`):

- `wave-enumeration-failed`
- `wave-summary-failed`
- `wave-ideas-failed`
- `starved-claims` (Phase 3, both paths)

Failure semantics per wave, deliberately NOT uniform:

| wave | on failure | reason recorded | why |
|---|---|---|---|
| 1, enumeration | `enumeration = None`, wave 2 still runs | `wave-enumeration-failed` | `null` is already a legitimate answer for most videos. A failed detection is not worth sinking a note over. Wave 2 loses only its dedupe hint. |
| 2, summary | fatal, `fallback_distilled` | `wave-summary-failed` (replaces the bare `fabric-error` / `yaml-parse-error` so the failing wave is named) | a note with no summary is not a note |
| 2, ideas | keep summary + enumeration, drop claims and key_ideas | `wave-ideas-failed` | partial beats nothing, and the reason makes it visible |

EVERY failed wave appends its name to `wave_failures`, unconditionally.
Separately, `ValidationMeta.fallback_reason` holds ONE string and takes the
most-destructive per the existing rule at `video.rs:391-397`
(`reduce-selection-failed` beats `partial-chunk-failure`):

```
wave-summary-failed  >  wave-ideas-failed  >  wave-enumeration-failed
```

`SummaryYaml` is shared by both paths. The reduce variant simply returns no
`tags` and no `links` (the reduce pattern is forbidden from emitting them,
`distill-video-reduce.md:132-133`), which the `Option` fields already allow.

### Wiring that must not be dropped

- All three short-path waves take `compose_capture_input(transcript,
  capture_note)` (`video.rs:171`), NOT the bare transcript. The operator's
  capture note is part of the distiller context and applies to every wave.
- The empty-transcript short-circuit (`video.rs:157`) stays FIRST, before any
  wave, so an empty transcript still burns zero fabric calls.
- `enforce_bounds` and `validate_anchors` continue to run once on the MERGED
  result in `distill()` (`video.rs:125,137`), not per wave. Claim caps and
  anchor stripping are unchanged.
- The key_ideas dedupe drops the ENTIRE `key_ideas` entry whose bolded theme
  name matches an enumeration item name, case-insensitive. A `key_idea` is a
  single formatted string (`**Theme** - explanation`), not a struct, so there is
  no partial to keep; renaming it would fabricate a theme the model did not
  write. Phase 0 measured 0 such collisions when wave 1's items are passed to
  wave 2, so this is a backstop, not the primary mechanism.

## Implementation Plan

### Phase 0: Prove the budget numbers, record the eval baseline
**Status:** DONE 2026-08-30. Fat-pattern and sibling measurements in the first
pass; narrow-prompt measurements added after review-panel round 1.
**Model:** sonnet
- Zero code. Call the Messages API directly with the real pattern files and real
  transcripts, read `stop_reason` and `output_tokens_details.thinking_tokens`.
- **Success criteria:** short path reproduces `stop_reason: max_tokens` at 4096
  and `end_turn` at 16384. OBSERVED: both, table above.
- **Success criteria:** long path reduce call measured at 4096. OBSERVED:
  `stop=end_turn out=2368 think=255 yaml_ok=True` on a 4,623-char reduce input
  (3 chunks). Headroom ~1,700 at that size. CAVEAT: the largest reduce input
  observed in production on 2026-08-30 was 7,129 chars, and the one recorded
  reduce truncation (`ht-99d3c5`) proves the headroom does run out. The spike
  measured a safe case, it did not establish a safe ceiling.
- Measure the sibling distillers at their maximum single-call input, to decide
  whether they belong in scope. All four share `SINGLE_CALL_TOKEN_THRESHOLD`
  (12,000 tokens, imported from `video.rs:38`), so max single-call input is
  ~48,000 chars for every kind.
- **Success criteria:** every sibling clears 4,096, or it joins the scope.
  OBSERVED, `claude-sonnet-5` at `max_tokens=4096`:

  RE-MEASURED 2026-08-30 against named fixtures. The first pass's numbers were
  real but its corpus was never recorded, which review-panel cheap-win #10
  flagged; rather than reconstruct identifiers after the fact, every row below
  was re-run from a fixture a reader can open. Each is the LARGEST `source.md`
  for its kind, truncated to the 48,000-char single-call maximum where longer.

  | kind | fixture (`config/eval/distill-fixtures/...`) | input | stop_reason | output | thinking | headroom |
  |---|---|---|---|---|---|---|
  | `article` | `article/www-theregister-com-ai-ml-2026-05-20-amd-says-its-4k-ryzen-a` (191,912 ch, truncated) | 48,000 ch | `end_turn` | 659 | 0 | 3,437 |
  | `session` | `session/slack-cli-release-promote` | 2,164 ch | `end_turn` | 1,043 | 10 | 3,053 |
  | `thread` | `thread/x-com-vllm-project-status-2059344804295942513` | 4,062 ch | `end_turn` | 1,177 | 0 | 2,919 |
  | `voicenote` | `voicenote/retrieval-cache-idea` | 1,366 ch | `end_turn` | 889 | 10 | 3,207 |

  VERDICT: all four stay out of scope. The discriminator is thinking spend, not
  output size. Video is the ONLY kind where the model deliberates; every sibling
  spent 0 to 10. `thread` | `session` | `voicenote` carry no `enumeration` and no
  `key_ideas` at all, so they are structurally lighter. `article` carries both
  and is the structural twin. A video transcript has no structure, so the
  pattern's "procedural asides are NOT an enumeration" gate is what burns the
  budget.

  GAP, stated rather than papered over: the first pass claimed an `article` row
  on a "real 10-item listicle with enumeration populated." No such fixture
  exists - all five `article` fixtures return 0 enumeration items, which is the
  same coverage gap already recorded under Parked. That row could not be
  reproduced and has been dropped rather than re-asserted. So the `article`
  verdict rests on prose articles only, and the Non-Goals revisit condition for a
  listicle `article` is doing real work, not hedging.
- **Success criteria (ADDED after review-panel round 1):** author the three
  short-path narrow patterns as scratch files and measure each at
  `max_tokens=4096` on a real transcript, recording `stop_reason` and
  `thinking_tokens`. The prior draft's budget table was extrapolated from the FAT
  pattern; nobody had measured a narrow one. OBSERVED, and it FALSIFIED the
  split-alone design: `-enumeration` at default thinking hit `stop_reason:
  max_tokens` on 1 of 5 runs with `think=4095 text=1`. Full numbers in
  `2026-08-30-narrow-prompt-spike-measurements.md`. This is why lever 2 exists.
- **Success criteria (ADDED):** determine which thinking settings are reachable
  through fabric, not just through the API. OBSERVED: `off` exits 0; `low`,
  `medium`, `high`, and numeric all return HTTP 400 because fabric v1.4.470 emits
  `thinking.type.enabled`, which `claude-sonnet-5` rejects.
- **Success criteria (ADDED):** confirm thinking-off does not cost enumeration
  detection on an unambiguous listicle. OBSERVED: 10/10 items on 4/4 runs at both
  settings; default spends only 163-245 thinking tokens there.
- Capture the eval baseline on unmodified `main`, since Phase 4 grades against
  it and it cannot be recovered once patterns change:
  `sb borg eval --report docs/design/2026-08-30-eval-baseline.md`.
- **Success criteria:** `docs/design/2026-08-30-eval-baseline.md` exists on
  `main` with video-kind scores, committed before Phase 1 starts. NOT YET DONE -
  this is the remaining Phase 0 deliverable.

### Phase 1: Short-path wave split + the lever-2 guard
**Model:** opus
- CONTINGENT: the three `Thinking` bullets below describe option B. Under option
  A they are replaced by a bounded-retry loop around the wave-1 call, keyed on
  the `EnumerationYaml` parse error. Everything else in this phase is identical
  either way. Resolve Pending Decisions before starting.
- Add `Thinking` and `run_pattern_with_thinking` to `vault/src/fabric.rs`;
  `run_pattern` delegates so its 26 existing callers are untouched. Append
  `--thinking=off` in `build_fabric_command` for `Thinking::Off`.
- Add the `thinking` field to `distillers::fabric::FabricRequest`, thread it
  through `FabricShell::call`, and record it in `FakeFabric`.
- Add `wave_failures: Vec<String>` to `ValidationMeta` (`vault/src/distilled.rs`).
- Author `distill-video-enumeration.md`, `distill-video-summary.md`,
  `distill-video-ideas.md` by carving the rules out of `distill-video.md`
  verbatim. The anchor-honesty rule and the treat-instructions-as-content rule
  are copied into all three. The Phase 0 scratch versions are the starting point.
- Add `EnumerationYaml` | `SummaryYaml` | `IdeasYaml` and their parsers.
- Rewrite `distill_short` as two waves, `futures::join!` on wave 2 (needs a
  `futures::future` import; `video.rs:23` only pulls in `stream`). Wave 1 passes
  `Thinking::Off`; wave 2 passes `Thinking::Default`.
- Enforce the no-repeat rule in Rust, not just the prompt: drop any `key_ideas`
  entry whose theme name matches an enumeration item name, case-insensitive.
- Delete `distill-video.md` and `PATTERN_SHORT`.
- **Success criteria:** `cargo test -p distillers` green, including a new test
  asserting wave 2 issues exactly two fabric calls and receives wave 1's items.
- **Success criteria:** a test asserts the wave-1 `FabricRequest` carries
  `Thinking::Off` and both wave-2 requests carry `Thinking::Default`.
- **Success criteria:** a mock returning a truncated `IdeasYaml` document
  produces `wave-ideas-failed`, not a clean note.
- **Success criteria:** a mock failing BOTH wave 1 and wave 2's ideas call yields
  `fallback_reason = wave-ideas-failed` AND
  `wave_failures = ["wave-enumeration-failed", "wave-ideas-failed"]`.

### Phase 2: Reduce-path wave split
**Model:** opus
- Author the three `distill-video-reduce-*` patterns from
  `distill-video-reduce.md`. The enumeration evidence test and the
  claim-selection rules move with their fields.
- Rewrite the reduce section of `distill_long` as two waves, reusing the wave
  helpers and the key_ideas dedupe from Phase 1. Wave 1 takes NO thinking lever:
  measured N=8 at both settings, 8/8 detection, worst total 774 of 4,096.
- Delete `distill-video-reduce.md` and `PATTERN_REDUCE`.
- **Success criteria:** `cargo test -p distillers` green; existing
  `reduce-selection-failed` behavior preserved by test.
- **Success criteria:** map step untouched, asserted by a test that counts chunk
  calls at N for an N-chunk transcript.

### Phase 3: Fail loudly on a starved parse
**Model:** sonnet
- Promote the existing warn-only guard (`video.rs:494`) to a recorded
  `fallback_reason` of `starved-claims` when claims are empty on a transcript
  over 500 words. This is the case a required serde field CANNOT catch: an
  explicit `claims: []` parses fine.
- Move it from `build_distilled` (short-path only, called at `video.rs:173`) to
  the merge point in `distill()`, after `enforce_bounds`, mirroring
  `mark_enumeration_shortfall` at `video.rs:143`, so it covers a reduce that
  selects nothing too.
- **`starved-claims` sets `fallback_reason` ONLY when it is currently `None`.**
  A run that already recorded `wave-ideas-failed` and therefore has zero claims
  must keep the real cause; relabelling it `starved-claims` would erase the
  failing wave. The existing precedence block at `video.rs:390-397` cannot
  clobber because it builds a fresh `ValidationMeta::default()`; this new site
  can, so the rule is explicit.
- **Success criteria:** a test breaking the code proves the guard bites: a
  distillation with 0 claims on a 600-word transcript carries
  `fallback_reason = starved-claims`.
- **Success criteria:** a test asserts a long-path (reduce) distillation with 0
  claims also carries it, proving the guard moved off the short path.
- **Success criteria:** a test asserts a run with `wave-ideas-failed` and 0
  claims keeps `fallback_reason = wave-ideas-failed`, not `starved-claims`.

### Phase 4: Pattern deployment and pruning
**Model:** sonnet
- Add all six new patterns to `PATTERNS` in `sb/src/cli/bootstrap.rs:88-99`, and
  remove the two deleted entries. The doc previously never named this file;
  patterns deploy from the binary's embedded list, not from `borg/patterns/`.
- `extract_canonical_assets` (`bootstrap.rs:253-263`) writes listed files but has
  no removal pass, so the two deleted patterns would linger on every existing
  host and the "exactly 7" criterion would read 9. Add a prune pass that deletes
  installed `distill-video*.md` files absent from `PATTERNS`.
- Scope the prune narrowly: it deletes only files matching a name the workspace
  previously shipped and no longer does. It must never delete an unrecognized
  file a user dropped into `~/.config/sb/patterns/` by hand.
- Extend the `sb doctor` patterns check (`sb/src/cli/checks.rs:392`), which today
  only reports missing/drift for listed files, to also Warn on a stale
  workspace-owned pattern that `PATTERNS` no longer contains.
- **Success criteria:** `sb bootstrap --force` on a host that currently has the
  three old patterns leaves exactly 7 `distill-video*.md` files installed.
- **Success criteria:** a test asserts the prune leaves an unrecognized
  hand-authored pattern file untouched.

### Phase 5: Replay and measure
**Model:** sonnet
- Replay by TRACE id - `sb borg replay` takes a `trace_id`, not a video id
  (`sb/src/cli/borg.rs:333-335`). The mapping, resolved from the staged
  `body.txt` of each trace:

  | video | trace | path |
  |---|---|---|
  | `iKwPaB5TUdI` | `ht-1b3f7659` | short |
  | `DWWrLlM3gwQ` | `ht-73eeb2e3` | short |
  | `DN2mhf0b02s` | `ht-4636d52d` | short |
  | `f8cfH5XX-XU` | `ht-c096e5e2` | long, 5 chunks, the control |

- **The eval harness does not invoke a distiller.** `borg/src/eval/fixtures.rs:41-80`
  loads a fixture only when the directory has BOTH `source.md` and a checked-in
  `distilled.yml`, and scores the checked-in artifact; `grep -rn "Distiller\|distill("
  borg/src/eval/*.rs` returns nothing. So running `sb borg eval` before and after
  scores identical static YAML and can never see this change.
- **The obvious repair is worse than the disease.** An earlier revision of this
  phase said "copy each replay's `distilled.yml` over the corresponding video
  fixture." There IS no corresponding fixture: `grep -rl` for all four video ids
  across `config/eval/` returns nothing, and the six committed video fixtures are
  six different videos. Worse, `borg/src/eval/judge.rs:140` builds the judge
  prompt as `# SOURCE {source}` + `# DISTILLED NOTE {note}` from the SAME fixture
  directory, so pasting video Y's distillation into fixture X grades Y's note
  against X's transcript. Faithfulness, coverage, and anchor-validity would all
  score noise. That is worse than a blind gate: it reports confident wrong
  numbers.
- **So the gate requires new fixtures, which is a corpus decision, stated here
  rather than smuggled in.** Add four NEW video fixtures, one per replayed trace,
  each carrying its own `source.md` (the staged transcript) alongside the
  `distilled.yml` the replay produced. Then:
  1. Replay the four traces by trace id.
  2. Write each into its OWN new fixture directory, `source.md` and
     `distilled.yml` together, so the judge grades a matched pair.
  3. Run `sb borg eval --fixtures config/eval/distill-fixtures` and compare the
     video axis against `docs/design/2026-08-30-eval-baseline.md`.
- The baseline was recorded over the SIX pre-existing fixtures, so the comparison
  is per-fixture on those six, and the four new ones are additive evidence rather
  than part of the before/after delta. Mixing them into an aggregate would move
  the mean for reasons unrelated to this change.
- **Success criteria:** all four replays land `fallback=none` with non-zero
  claims on the first attempt. The enumeration item COUNT is deliberately not
  asserted: Phase 0 measured the gate flipping between items and `null` on a
  borderline video across all thinking settings, and `null` produces no fallback.
- **Success criteria:** video-kind eval scores, computed over the REPLACED
  fixtures, are not below the recorded baseline.
- CAVEAT, recorded not fixed: `config/eval/distill-calibration.yml` does not
  exist, so the judge renders as uncalibrated (`borg/src/eval/report.rs:216`).
  Scores are comparable to each other but are not absolute.

## Acceptance Criteria

- [ ] `ls ~/.config/sb/patterns/distill-video*.md | wc -l` returns exactly 7
      after `sb bootstrap --force` on a host that previously had the old set.
      **Observed on main:** `3`
- [ ] `grep -c 'PATTERN_SHORT\|PATTERN_REDUCE' distillers/src/video.rs` returns
      exactly 0. **Observed on main:** `4`
- [ ] `grep -c 'possible pattern drift' distillers/src/video.rs` returns 0, and a
      named fallback reason replaces the bare warn.
      **Observed on main:** `1` (at `video.rs:494`, warn-only)
- [ ] `cargo test -p distillers` passes with at least 38 tests in
      `distillers/src/video/tests.rs`. **Observed on main:** `31`
- [ ] On a host carrying the three old patterns, `sb bootstrap --force` leaves
      exactly 7 `distill-video*.md` installed AND leaves a hand-authored
      `~/.config/sb/patterns/scratch-mine.md` untouched. **Observed on main:**
      leaves 3 and has no prune pass at all (`grep -c 'remove_file'
      sb/src/cli/bootstrap.rs` = `0`).
- [ ] A test drives a distillation where wave 1 AND wave 2's ideas call both
      fail, and asserts `fallback_reason == "wave-ideas-failed"` while
      `wave_failures == ["wave-enumeration-failed", "wave-ideas-failed"]`.
      **Observed on main:** cannot pass; the field does not exist
      (`grep -c 'wave_failures' vault/src/distilled.rs` = `0`).
- [ ] Replaying traces `ht-1b3f7659`, `ht-73eeb2e3`, `ht-4636d52d` yields
      `fallback=none` with `claims>0` on the FIRST attempt for each.
      **Observed on main:** cannot pass; `DWWrLlM3gwQ` (`ht-73eeb2e3`) failed 11
      consecutive replays on 2026-08-30.

Four presence-only greps (`"distill-video…"` count in `bootstrap.rs`,
`remove_file`, `thinking` in `vault/src/fabric.rs`, `wave_failures` in
`distilled.rs`) were dropped in favor of the behavioral assertions above. Each
was satisfiable by writing the token in a comment, and each duplicated a test
the Implementation Plan already requires. A criterion a comment can pass is not
a criterion.

One further criterion is PENDING the unresolved decision recorded below (which
lever guards the short-path enumeration call), because its wording depends on
whether `Thinking::Off` exists at all.

## Pending Decisions

**This section MUST be empty before Phase 1 starts** (`taste.md:25-27`). It has
one entry, so the doc is NOT ready to build.

- **What guards the short-path enumeration call?** Lever 2 (`--thinking=off`) was
  adopted after round 1 and then undermined by the round-2 N=12 measurement
  above. Two candidates, both measured:
  - **A. Drop lever 2; keep default thinking; add bounded retry on truncation.**
    A truncated wave 1 is loud (required-field parse error), so it is retryable.
    Measured 4/12 truncation per attempt, so three attempts leave ~3.7% residual.
    Detects better than `off` (7/12 vs 4/12) and adds no silent mode. Revives
    Alternative 4, which was rejected on the FAT pattern's 0-for-11 record; the
    narrow prompt's per-attempt success rate is materially different. Costs
    latency and a re-sent input on roughly one short-path video in three.
  - **B. Keep lever 2; add an `enumeration-absent` signal.** A null enumeration
    on a long transcript gets recorded the way `starved-claims` records empty
    claims. Fixes the silence but not the detection drop, and buys observability
    of the worse outcome. No retry cost; wave 1 gets faster.
  - Both leave the reduce path alone.

## Resolved Decisions

- **2026-08-30, Scott:** cover the long path as well as the short path. The
  first draft of this doc called Phase 2 prophylactic on the strength of a
  synthetic measurement. That was wrong: `ht-99d3c5` (2026-08-08) is a real
  reduce truncation with the identical signature. Phase 2 is a bug fix on a
  lower-frequency instance of the same defect.
- **2026-08-30, Scott:** wave shape is 1 then 2. Enumeration alone first, then
  summary and ideas in parallel.
- **2026-08-30, Scott:** measure the sibling distillers rather than park them on
  suspicion. Measured (Phase 0 table): all four clear 4,096, so they stay out of
  scope on evidence. The spike also produced the discriminator that explains WHY
  video is the outlier: it is the only kind where the model spends thinking
  tokens, because a transcript has no structure for the enumeration gate to read.
- **2026-08-30, after review-panel round 1: SUPERSEDED by round 2.** This
  recorded adopting `--thinking=off` on the enumeration call, reversing the
  blanket rejection of Alternative 2. Round 2's N=12 re-measurement showed the
  lever detects LESS on the ambiguous case (4/12 vs 7/12) and converts a loud
  truncation into a silent `enumeration: null` (3/12 to 8/12). The decision is
  reopened as the one Pending Decision. Kept rather than deleted, because the
  reasoning below is still the reasoning that makes option B viable. The
  rejection was correct for the FAT pattern, where the job wanted 2,860 text
  tokens and thinking-off bought only ~1,200 of headroom. The split changes that
  arithmetic: the narrow enumeration call wants ~1,272 tokens, so thinking-off
  leaves 2,824 spare. The two levers are complementary, and Phase 0 proved
  neither is sufficient alone.
- **2026-09-05:** the `scottidler/Fabric` fork with `--maxTokens` is accepted as
  the interim guard while the wave-split design stays Draft. Evidence:
  `borg.yml:63-75` (dotfiles) carries Scott's comment naming
  `scottidler/Fabric` (fork of v1.4.473, fixes open as
  `danielmiessler/Fabric#2207`), the mise re-sync caveat, and `sb doctor` as
  the guard, plus `max-tokens: 16384`; every `fabric` on desk reports v1.4.473
  with the flag. Alternative 1 stays rejected as the *final* fix.

## Alternatives Considered

### Alternative 1: Raise fabric's max_tokens
- **Description:** patch `anthropic.go`, or add the missing `--max-tokens` CLI
  flag upstream. It would mirror `ModelContextLength` exactly: declared at
  `internal/cli/flags.go:51`, wired into `ChatOptions` at `:463` (line numbers
  from upstream `main`, fetched 2026-08-30).
- **Pros:** fixes the class for every fabric user, one call stays one call.
- **Cons:** the flag does not exist in v1.4.470 or on `main`, so it means running
  a local build until a release lands it.
- **Why not chosen:** Scott rejected carrying a fabric fork.

### Alternative 2: Pass `--thinking off`
- **Status:** PENDING. Adopted after round 1, reopened after round 2's N=12
  measurement. It survives as option B of the one Pending Decision, and only
  paired with an absent-enumeration signal that makes its silent failure mode
  observable.
- **Description:** one line in `build_fabric_command` (`vault/src/fabric.rs`).
- **Why it was rejected as a standalone fix:** it buys ~1,200 tokens of headroom
  on a job that wants 2,860, so a longer video breaks it again. That reasoning
  still holds for the unsplit pattern.
- **Why it is adopted alongside the split:** once the job is narrow, the
  enumeration call wants ~1,272 tokens, and thinking-off leaves 2,824 spare. The
  second objection - that it gives up reasoning on a pattern with a hard
  enumeration gate - was tested rather than assumed: detection is 10/10 on 4/4
  runs at both settings on an unambiguous listicle, and on the ambiguous case the
  extra deliberation does not produce a stable answer anyway.
- Applied to wave 1 only. Waves 2 and 3 keep default thinking, where they spend
  0-38 and 343-787 tokens respectively and have 3,586 / 2,116 spare.

### Alternative 3: Drive fabric's REST server
- **Description:** `fabric --serve`, POST `/chat` with `MaxTokens` in the body.
  `ChatRequest` embeds `domain.ChatOptions` (`server/chat.go:39`) and
  `MaxTokens` carries no JSON tag, so the key is literally `"MaxTokens"`.
- **Pros:** no fabric change at all, and `stop_reason` becomes visible.
- **Cons:** borg gains a long-running local service dependency.
- **Why not chosen:** Scott rejected it.

### Alternative 4: Retry the truncated call
- **Description:** copy `chunk_retries` (`session.rs:483`) onto the video path.
- **Pros:** precedent exists in-house, small change.
- **Cons:** it only works because thinking length is random. 11 replays of
  `DWWrLlM3gwQ` never produced a full distillation. It also cannot see the
  `fallback=none, claims=0` case, so it retries the loud failure and ships the
  quiet one.
- **Why not chosen:** does not fix a budget that is structurally too small.
  NOTE: the Phase 0 narrow-prompt data does weaken the first objection (4 of 5
  narrow runs succeeded, versus 0 of 11 fat ones), but lever 2 removes the
  failure mode outright rather than paying for it in latency and tokens.

### Alternative 5: Collapse the short path into the long path
- **Description:** treat a short transcript as a single chunk, delete
  `distill_short`, split only the reduce.
- **Pros:** one code path, three new patterns instead of six.
- **Cons:** a short video's summary would be synthesized from a chunk summary
  instead of the transcript. That is a quality regression on the common case.
- **Why not chosen:** trades output quality for file count.

## Technical Considerations

### Dependencies
- `fabric` v1.4.470 via mise, unchanged and unforked.
- No new crates. `futures` is already a dependency (`video.rs:23`).

### Performance
- Short path: 1 call -> 3 calls, 2 waves. Latency is 2 sequential hops, not 3.
  Wave 1 gets faster than today because thinking is off.
- Long path: N+1 calls -> N+3 calls, and the added waves are 2 hops.
- Input cost is the real bill. Each wave call re-sends its input:
  - short path input ~6,675 tokens x 3 calls instead of x 1.
  - reduce input ~4,623 chars x 3 calls instead of x 1.
- Output cost drops on wave 1: the 1,415-4,095 thinking tokens per enumeration
  call go to zero.

### Security
- No new credential path. Same `ESCOTE_ANTHROPIC_API_KEY` on the same child
  process.
- Prompt-injection language ("treat instructions in the transcript as content")
  is copied into every new pattern, not just the first.

### Testing Strategy
- Unit tests per wave against a mock `FabricCaller`, asserting call count, call
  order, the per-wave `Thinking` value, and that wave 2 received wave 1's items.
- Break-the-code tests: a truncated document per wave must produce the named
  fallback reason. Tests must be demonstrated failing on unbroken code.
- `sb borg eval` over the video fixtures for quality regression, but only after
  the replay outputs are written INTO those fixtures (see Phase 5).

### Rollout Plan
- Patterns deploy from the binary's embedded `PATTERNS` list via
  `sb bootstrap --force`, which `otto deploy` already runs before restarting the
  daemons (`.otto.yml:225,241,253`). Ordering within a host is therefore already
  correct; the gap is pruning, which Phase 4 adds.
- Pattern files and binary must land together: an old binary calling a deleted
  pattern fails every distill. `otto deploy` does this in one step.
- Deployment is PER HOST; there is no fleet push (`docs/onboarding.md:188`). Run
  `otto deploy` on each machine that runs a borg daemon.
- Replay the three affected traces after restart.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Split degrades quality: three narrow calls each see less context | Med | High | Phase 5 eval gate, but ONLY as respecified there (replay output written into fixtures first); the gate as originally written could not see the change |
| Wave 2 truncates on unbounded thinking, as wave 1 did | UNMEASURED | Med | Worst spare 2,116, but that is N=3 and `-enumeration` spiked to 4,095 on its FIRST run, so a small sample proves little about a heavy tail. Not rated Low: the honest statement is that nobody has run enough wave-2 samples. The required-field serde structs do make it loud if it happens (`distilled.rs:398` to `checks.rs:575`), which is why this is Med and not High |
| Pattern/binary skew, or stale deleted patterns lingering | Med | High | `otto deploy` handles ordering; Phase 4 adds the prune pass and the doctor warning |
| Wave 2 ideas call ignores wave 1 items and repeats them | Low | Low | Measured 0 repeats; Rust dedupe as backstop |
| A dense 25-30 item video still exceeds budget | Low | Med | Extrapolated ~1,950 tokens with thinking off, ~2,100 spare; unproven, see Open Questions |
| A future pattern grows past 4,096 again | Med | Med | Phase 3 guard makes it loud instead of silent |
| Input cost triples on the short path | High | Low | Accepted; output quality is the thing being bought |

## Open Questions

None.

`~/repos/.claude/rules/taste.md:25-27` is explicit that ready-to-build means Open
Questions EMPTY. A question the author has decided not to answer is not an open
question, it is an accepted risk, and it belongs in Parked with a revisit
condition where it carries a trigger instead of sitting in a list that blocks the
gate. Both items previously parked here have moved down accordingly; neither was
deleted.

## Parked, with revisit conditions

- **The 30-item enumeration extrapolation is unproven.** The ~1,950-token figure
  is measured per-item cost (~65 tokens) times the supported cap, not a measured
  30-item run; no 25+ item video exists in the corpus. ACCEPTED because the
  Phase 3 guard makes a miss loud rather than silent. Revisit when a video with
  more than ~20 enumerated items is ingested.
- **The eval judge is uncalibrated.** `config/eval/distill-calibration.yml` does
  not exist, so `borg/src/eval/report.rs:216` renders scores as uncalibrated.
  ACCEPTED for Phase 5, whose gate is a before/after comparison from the same
  judge and therefore internally consistent; absolute scores are meaningless and
  must not be quoted as such. Revisit when a calibration set is authored.
- **The reduce-path enumeration call needs no thinking lever.** Measured N=8 at
  both settings on a real 7,327-char reduce input: 8/8 detection, 3 items every
  run, worst total 774 of 4,096 at default thinking. It reads a pre-extracted
  candidate list carrying `Declared count: 3`, so its gate has explicit evidence
  and the model does not deliberate. Revisit if a reduce input ever arrives with
  candidates but no declared count and no ordinals.
- **Sibling distillers.** Out of scope on measured headroom, not assumption.
  Revisit when: a listicle `article` exceeds ~20 enumerated items, or any
  `article` | `thread` | `session` | `voicenote` trace logs `yaml-parse-error`
  or `fallback=none` with `claims=0`.
- **Bounded thinking effort instead of off.** `claude-sonnet-5` supports
  `thinking.type.adaptive` + `output_config.effort`, and Phase 0 measured
  `effort: medium` at 588-1,148 thinking tokens with 19/19 detection on the runs
  where the gate fired - strictly better than `off`. It is unreachable because
  fabric v1.4.470 emits the legacy `thinking.type.enabled` shape and gets a 400.
  Revisit when fabric ships adaptive-thinking support.
- **The enumeration gate is nondeterministic on borderline videos.** The same
  transcript yielded 19 items or `enumeration: null` across 24 runs at four
  thinking settings. Pre-existing, unchanged by this work, and `null` produces no
  fallback. Revisit if a video that clearly declares a count starts returning
  `null`.
- **Root-cause granularity inside `wave-summary-failed`.** The label collapses
  `fabric-error`, timeout, and YAML truncation into one string. `wave_failures`
  carries the set of failing waves but not the reason each failed. Revisit if an
  operator hits one at 3am and the label is not enough.
- **Eval coverage gap for article enumeration.** All five `article` fixtures
  predate the 2026-07-07 enumeration feature and none has a populated
  `enumeration:` block, so `sb borg eval` cannot currently catch an article
  enumeration regression. Not this doc's problem to fix; recorded so it is not
  rediscovered.

## References

- `distillers/src/video.rs`, `distillers/src/parse.rs`, `vault/src/fabric.rs`
- `distillers/src/fabric.rs` (the `FabricCaller` port and `FabricRequest`)
- `sb/src/cli/bootstrap.rs` (embedded `PATTERNS`), `sb/src/cli/checks.rs`
- `borg/src/eval/fixtures.rs` (why the eval gate needed respecifying)
- `borg/patterns/distill-video{,-chunk,-reduce}.md`
- `docs/design/2026-08-30-narrow-prompt-spike-measurements.md` (Phase 0 raw data)
- `docs/design/2026-07-24-harvest-distill-parsing-robustness.md` (same symptom,
  different root cause: duplicate keys, not truncation)
- fabric `internal/plugins/ai/anthropic/anthropic.go:54,238-240` (v1.4.470)
- fabric `internal/cli/flags.go:51,463` (the flag that would need to exist)
- Review-panel round 1 synthesis: `/tmp/review-panel/4XQnynJt/synthesis.md`
