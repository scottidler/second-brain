# Design Document: Distiller Long-Source Claim Fidelity

**Author:** Scott Idler
**Date:** 2026-10-18
**Status:** Draft
**Review Passes Completed:** 5/5 on the reworked design (draft, correctness, clarity, edge cases, excellence); panel round 1 folded into the earlier chunk-repair design, which this rework replaces

## Summary

Long-source notes carry claims their own source does not back. Two independent judges flag the same original claim on 6 of 29 long-source notes and 1 of 35 short ones. I checked the six by hand. Two causes come from the distiller cutting long sources up: session windowing drops the middle, and the reduce step rewrites claims without the source. That cutting rests on a 12,000-token "single call" line copied from a 2026-05-16 design, never measured and never a model limit. The largest staged source on ripr is 2.49M chars (about 620k tokens), inside a 1M-token window. Send every source to the model in one call, delete the chunk/reduce path, add a claim-honesty rule for the third cause, and measure on the notes the judges already flagged. This blocks `docs/design/2026-10-17-claim-judge-full-source.md` Phase 0.

## Problem Statement

### Background

- The claim-judge Phase 0 run (2026-10-18, ripr) planted a number swap into notes and asked two judges (claude-sonnet-5, gemini-2.5-pro) to judge the original and the mutant.
  - The swapped claim flipped to not-supported in 25/27 (claude) and 26/27 (gemini) long-source pairs.
  - The paired metric still failed (0.59 / 0.57 against 0.80), because 11-12 of 29 long-source originals were already flagged.
- Scott's call (2026-10-18): fix the distiller first, then re-run that Phase 0. Test the fix on the notes that are already flagged.
- Scott (2026-10-18): "llms have a 1million token window. why the fuck would it take 20 or 100 llm calls?"
- Where the 12,000-token line came from:
  - `docs/design/2026-05-16-extractor-contract-and-l2-summaries.md:592-595`: "YouTube uses a two-stage shape when transcript exceeds 12K tokens", with no measurement behind the number.
  - `distillers/src/video.rs:36-38`: `SINGLE_CALL_TOKEN_THRESHOLD = 12_000`, "Calibrated against the design doc's guidance". Article, thread, session and voicenote import it.
- Distill routing today:
  - At or below 12,000 tokens, one call.
  - Above it, article | video | thread | voicenote run chunk -> map -> reduce, with 8,000-token chunks.
  - Repo is always one call.
  - Session windows the body head+tail to `token_cap` (default 12,000, `distillers/src/session.rs:129`, `borg/src/config/harvest.rs:50`) before routing. No host raises it: ripr's `~/.config/sb/borg.yml` has no `token-cap` line.
- Other input caps on the single-call path:
  - `fabric.max-content-chars: 100000` on ripr (default 100,000, `borg/src/config.rs:1083`). `vault::fabric::run_command` head-truncates any input over it (`vault/src/fabric.rs:162-172`).
  - Per-call `timeout_secs` defaults to 60 in each distiller config (for example `distillers/src/video.rs:80`).
  - `max_claims` (`distillers/src/validate.rs:36-52`) allows 10 claims, plus 2 per extra chunk up to 24, so a single call on a long source gets only 10.
- Staged source sizes on ripr (2026-10-18, file sizes of `body.txt`, `transcript.md` and `distilled.yml`, approximate):

| over | 48k | 100k | 400k | 1M | 2M |
|---|---|---|---|---|---|
| files | 858 | 478 | 115 | 22 | 2 |

  The largest is `hv-a158c300` at 2,486,392 bytes, about 620k tokens.

### Problem

The six claims both judges flagged, checked against their staged sources:

| trace | kind | path | error | cause |
|---|---|---|---|---|
| `hv-29b2f05e` | session | windowed single call | "All three security reviews"; the source has 4 review prompts (byte offsets 59, 29,733, 41,685, 47,571) | windowing dropped chars 28,436-30,801, which held the second prompt. Rebuilt window = 11,859 tokens = `meta.input-tokens` exactly |
| `cl-2de2b08b` | video | chunk/reduce | "pulling the center to the backside linebacker"; source at 00:42:14 says "ID linebacker" | reworded at chunk or reduce; chunk outputs are not staged |
| `cl-0222aa9e` | video | chunk/reduce | the interviewer's hedged question ("I could be way off on this, so you tell me", 00:58:56) stated as a position | same; the judge also never sees `who` (`borg/src/eval.rs:410`) |
| `cl-2db85dab` | repo | single call, full input | "at least nine languages"; the table has 9 repos in 8 languages (zh-CN twice) | no rule on counting distinct items |
| `cl-9e724b4b` | article | single call, full input | merges the 3-Raspberry-Pi "production" cluster (char 193) with the six-node Proxmox/Vultr/OVH cluster (chars 25k-36k) | no rule against cross-section merges |
| `hv-16d375ce` | session | single call, full input | a "Reusable pattern:" lesson the session never states | `distill-session.md:52-53` asks for techniques that generalize |

Three causes:

- **Session windowing drops the middle.** `window_head_tail` (`distillers/src/session.rs:614-635`) cuts any body over 47,420 chars at the default cap, and nothing in `meta` records the cut. Two more of the 15 flagged traces saw 26-32% of their source (`hv-2fe2ec93` 147,632 bytes, `hv-5194d942` 184,203 bytes).
- **The reduce step writes claims without the source.** `build_reduce_input` (`distillers/src/parse.rs:725-748`) drops `kind`, `who` and `quote`. `select_reduce_claims` (`parse.rs:793-826`) takes claim text from the reduce output with no text check. All five reduce patterns say "You may lightly consolidate wording".
- **No claim-honesty rule.** Three of the six errors came from single calls that saw the whole source. No pattern rules on counting, cross-section merges, hedges, or unstated generalizations, and `distillers/src/validate.rs` checks structure only.

The first two exist only because the source is cut up. Neither is a model limit.

### Goals

- Every source reaches the distill model whole, in one call, up to one configured ceiling. (Scott, 2026-10-18)
- Above the ceiling, the cut is recorded and the receipt is marked degraded.
- Every claim is backed by one passage of the source. (Scott, 2026-10-18: "fix the distiller's long-source errors first")
- The fix is measured on the already-flagged long-source notes, against a same-code baseline. (Scott, 2026-10-18: "pick items that are already problematic")
- The claim-judge Phase 0 re-runs after this ships.

### Non-Goals

- **Excluded: transcript errors.** The judge checks a claim against the staged transcript, so a misheard word ("Grok" for Groq in `ht-37006094`) passes.
- **Excluded: frames and visual content.** None of the six errors involves visuals.
- **Excluded: the Gemini engine's empty first window** (`cl-0222aa9e` starts at `[00:30:00]`). It needs its own fix in `borg/src/youtube/gemini.rs`.
- **Excluded: `distill-image`.** Not a long-source path.
- **Parked: sending `who` to the claims judge.** Owned by the claim-judge doc's Parked list. Revisit if `cl-0222aa9e`'s claim still flags after this ships.
- **Parked: a pre-publish grounding check** on top of `quote_grounding` (`borg/src/eval/calc.rs:183`). Revisit if Phase 4 misses its bar.

## Proposed Solution

### Overview

1. Phase 0 proves one call works: the 14 test sources and the largest staged sources, distilled whole, judged on the full source, against the current chunked output.
2. One config ceiling replaces the 12,000-token line, the session `token_cap`, and the fabric input cap for distills.
3. Delete the chunk/reduce path and its ten patterns.
4. Add a claim-honesty rule to the single-call patterns.
5. Re-measure on the same notes and judges, with a per-error outcome.

### Architecture

```
today:  source -> [session: head+tail window at 12K] -> <=12K tokens ? one call : chunk xN -> reduce
after:  source -> <= single-call-max-chars ? one call
                                           : head+tail window to the ceiling, window: entry, degraded -> one call
```

- One path per kind. The capture note, which the map-reduce path drops today (`docs/design/2026-10-14-note-sources-intent-tools-channels.md:74`), now reaches every distill.
- The enumeration, anchor and command-honesty rules already live in the single-call patterns, so they keep working.

### Data Model

- New config key `distill.single-call-max-chars` in `borg.yml`, and the same key in `cortex.yml`, because `sb cortex summarize --backfill` builds its own `Dispatcher` (`cortex/src/summarize.rs:70`). Its default comes from Phase 0. It replaces:
  - `SINGLE_CALL_TOKEN_THRESHOLD` routing in every distiller;
  - session `token_cap` and `harvest.token-cap`, deleted with their template line (`config/templates/borg.yml.example:277`);
  - `fabric.max-content-chars` as the distill input cap. Distill calls pass the ceiling as `max_chars`; other fabric callers keep `max-content-chars`.
- Above the ceiling, the existing `window_head_tail` moves to `distillers/src/parse.rs` and serves every kind. It pushes `window:<source_chars>><kept_chars>` into `meta.validation.bounds-truncations`.
- `ValidationMeta::is_degraded()` (`vault/src/distilled.rs:444-446`) returns true on a `window:` entry, so the receipt is `degraded=true` and `sb doctor` warns.
- `max_claims` (`distillers/src/validate.rs:48`) takes source tokens instead of a chunk count: 10, plus 2 per 8,000 tokens past the first 12,000, ceiling 24. That matches what the chunked path allowed (10, plus 2 per extra 8,000-token chunk, ceiling 24).
- Per-call distill `timeout_secs` scales with input size: a base plus a per-100k-chars allowance, both config, values from Phase 0's measured latency. The pipeline hard timeout (1800s) stays the outer bound.

### API Design

- No CLI change.
- Deleted: `chunk_transcript`, `build_reduce_input`, `build_thread_reduce_input`, `select_reduce_claims`, every `distill_long`, `CHUNK_TOKEN_TARGET`, `SINGLE_CALL_TOKEN_THRESHOLD`, and the patterns `distill-{article,video,thread,session,voicenote}-{chunk,reduce}.md`.
- `borg/src/eval/candidate.rs:77` `reaches_single_call` becomes "at or under the ceiling", so `--candidate-pattern` can score long traces.

### Implementation Plan

#### Phase 0: Prove one call beats chunking (zero committed code)
**Model:** opus
- Operator-run on ripr with a throwaway build, the same isolation as the claim-judge Phase 0 (scratch `XDG_DATA_HOME`, scratch patterns dir, live DB row counts checked before and after).
- Test set: the 15 flagged traces, `cl-56dd5199 cl-9e724b4b cl-ad229f9f cl-b14d7e33 cl-2db85dab hv-12ae5acc hv-16d375ce hv-29b2f05e hv-2fe2ec93 hv-5194d942 cl-0222aa9e cl-1ed09284 cl-2de2b08b cl-33faf56c cl-38cd120a`. `hv-12ae5acc` and `hv-16d375ce` share one body (same md5), so 14 distinct sources.
- Size probes: `hv-a158c300` (2.49M bytes) and `cl-31951716` (1.42M bytes, the largest video).
- Copy all 17 trace dirs into the Phase 0 scratch tree first, so a `sb borg retention sweep` before Phase 4 cannot remove them.
- The throwaway build has two arms over the same traces:
  - **chunked:** current code, unchanged.
  - **one call:** threshold, `token_cap` and `max_chars` lifted, timeout 900s, claims budget scaled by source size.
- It re-distills named staged traces through the full `Dispatcher` (`borg/src/eval/regenerate.rs:199` is the precedent) and judges claims on the full source (the claim-judge Phase 0 patch).
- Distill each test source 5 times per arm, and each size probe once in the one-call arm. Judge every output with both judges. Retry an errored judge call once.
- Record per call: the served model, input tokens, wall time, and whether the call or the parse failed.
- **Success criteria:**
  - Both size probes complete in one call with the served model logged. If either fails on window size, the ceiling sits below it and the doc records the measured limit.
  - A table lands in this doc: per source, arm and run, both-judge flagged claims, claims per note, the fraction of anchored claims in the source's last third, and wall time.
  - Each of the 6 core errors is marked reproduced in the chunked arm (3+ of 5 runs) or baseline-unreproduced.
  - **Go rule** (one call ships only if all hold):
    - the one-call mean of both-judge flagged claims per run is at or below the chunked mean;
    - claims per note are within `eval.candidate.max-coverage-cut` of the chunked arm;
    - on sources whose claims carry anchors (videos), the last-third fraction is at least 0.8 of the chunked arm's (no head bias);
    - every test-source call finishes inside 1800s.
  - Live `eval_scores` and `judge_trust` row counts are unchanged after the run.
- If the go rule fails, this doc returns to Draft with Alternative 1.

#### Phase 1: One ceiling, one call
**Model:** opus
- Add `distill.single-call-max-chars` with the Phase 0 default, threaded through `Dispatcher` (`distillers/src/dispatcher.rs`) and `regenerate_dispatcher`.
- Every distiller takes the single-call path at or under the ceiling. Above it, head+tail window to the ceiling, record `window:`, mark degraded.
- Distill fabric calls pass the ceiling as `max_chars`. Scale `max_claims` and `timeout_secs` as in Data Model.
- Delete `harvest.token-cap` and session `token_cap`, and the template line.
- **Success criteria:**
  - `cargo test -p distillers long_source_is_one_call` passes: a 200,000-char article, video, thread, session and voicenote each make exactly one distill call through `FakeFabric`, with the whole input after truncation.
  - `cargo test -p distillers over_ceiling_records_window` passes with an explicit 1,000-char ceiling: the entry is present and `is_degraded()` is true. It fails with the push removed.
  - `cargo test -p borg regenerate_dispatcher_uses_single_call_ceiling` passes: `regenerate_dispatcher` built from a borg config with a 300,000-char ceiling distills a 200,000-char article in one call. It fails if the eval dispatcher falls back to the default.
  - `rg -n "token-cap" config/templates/borg.yml.example` prints nothing.

#### Phase 2: Delete the chunk/reduce path
**Model:** sonnet
- Delete the symbols and ten patterns listed in API Design, and their tests. `otto deploy` stops syncing the ten patterns.
- Update `reaches_single_call`, `distillers/AGENTS.md`, and the CLAUDE.md patterns count.
- **Success criteria:**
  - `rg -l "fn distill_long|select_reduce_claims|build_reduce_input|build_thread_reduce_input|SINGLE_CALL_TOKEN_THRESHOLD|CHUNK_TOKEN_TARGET" distillers borg --type rust | wc -l` prints `0` (13 on main). The search names distiller symbols only: `vault`'s embedding code has its own unrelated `chunk_transcript`, which stays.
  - `ls borg/patterns | rg -c "(chunk|reduce)\.md$"` prints nothing (no matches).
  - `otto ci` passes.

#### Phase 3: Claim-honesty rule
**Model:** opus
- One CLAIM-HONESTY block, the same text in `distill-article`, `distill-video`, `distill-thread`, `distill-session`, `distill-voicenote` and `distill-repo`:
  - count distinct items, not rows or mentions;
  - state no total over a truncated view;
  - a question or hedge is never restated as an assertion. An attributed opinion stays a position, with its qualifier;
  - one claim, one passage: never join facts from separate sections;
  - no lesson or generalization the source does not state;
  - pick claims from the whole source, early, middle and late.
- Narrow `distill-session.md:52-53` so a "reusable pattern" must be stated in the session.
- Score each of the six with `sb borg eval --candidate-pattern`, now on long traces too.
- **Success criteria:** each candidate run reports no coverage cut beyond `eval.candidate.max-coverage-cut`.

#### Phase 4: Re-measure (operator, ripr)
**Model:** opus
- The Phase 0 one-call arm, rebased on the shipped code, 5 runs per test source.
- Each core error that reproduced in Phase 0's chunked arm gets one outcome:
  - **reproduced:** the error appears in any run (this beats the other two);
  - **corrected:** the claim is present in 3+ of 5 runs and both judges call it supported;
  - **omitted:** the claim is absent in 3+ of 5 runs.
- A baseline-unreproduced error is reported, never counted as fixed.
- Missing verdicts:
  - an error seen in any run is reproduced, even when other runs of that source miss verdicts;
  - otherwise, a source with more than one run missing a verdict after the retry is inconclusive and leaves both counts;
  - the both-judge mean is divided by the runs with complete verdicts, never by 5.
- **Success criteria:**
  - Every countable core error is corrected or omitted. At least 3 of the 6 must be countable, or the result is inconclusive.
  - The mean both-judge flagged claims per run falls by at least half against Phase 0's chunked arm.
  - Claims per note stay within `eval.candidate.max-coverage-cut` of the chunked arm.

## Acceptance Criteria

- [ ] No distiller routes on the 12K line: `rg -l "SINGLE_CALL_TOKEN_THRESHOLD" --type rust . | wc -l` prints `0`.
  - Observed on main: `11`.
- [ ] No chunk or reduce pattern ships: `ls borg/patterns | rg -c "(chunk|reduce)\.md$"` prints nothing.
  - Observed on main: `10`.
- [ ] The template no longer pins a session cap: `rg -n "token-cap" config/templates/borg.yml.example` prints nothing.
  - Observed on main: `277:  token-cap: 12000`.
- [ ] A long source is one call: `cargo test -p distillers long_source_is_one_call` passes.
  - Cannot run yet: the test lands in Phase 1.
- [ ] Phase 4's table meets its bar: every countable core error corrected or omitted, and the both-judge mean down by half.
  - Cannot run yet: needs Phases 0-3.

## Resolved Decisions

- 2026-10-18 (Scott): fix the distiller before the claim-judge Phase 0 re-run, and test on the already-flagged notes.
- 2026-10-18 (Scott): one call over the whole source, not chunking. The 12,000-token line was never a model limit.
- 2026-10-18 (panel round 1, on the earlier chunk-repair design): findings on the reduce schema, the reduce index contract, the unframed session chunk path, and the session `token_cap` default sites are moot, because their code is deleted in Phase 2. Findings that carry over are folded:
  - the per-error outcome, reproduce gate and half-reduction margin, with 5 runs;
  - the degraded wiring for a `window:` cut;
  - the eval dispatcher reading the same config as production;
  - wall time against the 1800s timeout;
  - the hedge-rule wording and the explicit pattern list;
  - the deployed-version probe after ship.
- 2026-10-18 (panel round 2, on the pre-rework snapshot `1a5deb8`): the dispute closed for the author. Both seats confirmed all five reduce fallbacks set `reduce-selection-failed` (`session.rs:408`, `article.rs:493`, `video.rs:421`, `thread.rs:420`, `voicenote.rs:332`), which `is_degraded()` reads and every publish site ORs into the receipt. Moot after the rework, which deletes the reduce path. Two findings that carry over are folded: a borg-side test that the eval dispatcher uses the ceiling (Phase 1), and the missing-verdict precedence and denominator (Phase 4).
- 2026-10-18: the token-cap question sent to Scott is withdrawn. One ceiling replaces it, set from Phase 0.

## Alternatives Considered

### Alternative 1: Keep chunking, repair the reduce step
- **Description:** the round-1 design. Raise the session cap, frame session chunks as data, and have the reduce copy pool claims by index.
- **Pros:** keeps calls small; no reliance on long-context quality.
- **Cons:** keeps two paths per kind, ten patterns, a reduce parse contract, and claims that never see the whole source. It is all built on a 12,000-token number nobody measured.
- **Why not chosen:** one call removes both chunk-made causes at once. Phase 0 is the fallback trigger: if the one-call arm loses, this doc returns to this design.

### Alternative 2: Keep head+tail windowing, forbid totals in the session pattern
- **Description:** leave the 12,000 cap and add a rule against counting across the marker.
- **Cons:** `hv-5194d942` keeps 26% of its source. A rule cannot recover what the model never saw.
- **Why not chosen:** it fixes one symptom and leaves the coverage gap.

### Alternative 3: Pre-publish claim grounding check
- **Description:** score each claim against the source with shingles before publish.
- **Cons:** `quote_grounding` works on quotes; most claims carry none, and paraphrase grounding has no measured precision.
- **Why not chosen:** parked (Non-Goals) until Phase 4 shows the cause fixes are not enough.

## Technical Considerations

### Dependencies
- The claim-judge Phase 0 patch (scratchpad, not committed) for full-source judging in Phases 0 and 4.
- Blocks the claim-judge doc's Phase 0 re-run. Single repo; no cross-repo blast radius.
- The served distill model's window. Sessions run `claude-sonnet-5` (`llm.model` on ripr); other kinds run fabric's `DEFAULT_MODEL`, which I could not read (hook-blocked). Phase 0 logs the served model and proves the window with the size probes.

### Performance
- Input tokens per source stay about the same: chunking already sent every token once, plus a reduce call and a pattern prompt per chunk. One call drops the reduce and the repeated prompts.
- Latency per call rises with input size. Phase 0 measures it, and the timeout scaling uses the measurement.

### Security
- No new inputs, credentials or network surface. Deleting the session chunk path also deletes its unframed prompt input (`session.rs:495`), which skipped the bcbbf73 data framing.

### Testing Strategy
- Unit tests per phase, each shown to fail with its change removed.
- Tests read fabric's input after truncation, since `FakeFabric` records it before (`vault/src/fabric.rs:162-172` truncates later).
- Phases 0 and 4 are the end-to-end measure, 5 runs per source per arm, because LLM output varies run to run.

### Rollout Plan
- `bump` + `otto deploy`, which installs the binary and patterns in one task.
- After deploy, check the installed `sb --version` on ripr, then ingest one long session and one long video:
  - each receipt is not degraded, and `bounds-truncations` is empty;
  - each distill made one call (daemon log).
- New ingests take the new path. Old notes change only on `sb borg replay`.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Long-context quality is worse than chunking (lost in the middle) | Med | High | Phase 0's go rule compares both arms on the same traces; a loss sends the doc back to Alternative 1 |
| Head bias: one call picks claims from the start of a long source | Med | Med | the last-third measure in Phase 0, the "early, middle and late" rule in Phase 3 |
| The fabric default model has a smaller window | Low | High | Phase 0 size probes log the served model; the ceiling is set from what completes |
| A long call times out | Med | Med | Phase 0 measures wall time; the timeout scales with input; 1800s stays the outer bound |
| Deleting chunk/reduce removes a path something else uses | Low | Med | Phase 2's `rg` criterion plus `otto ci` |
| Run-to-run noise hides or fakes a fix | Med | Med | 5 runs, a reproduce gate, an inconclusive outcome, a half-reduction bar |

## Open Questions

None. The ceiling's value comes from Phase 0's measurement, not from a decision.

## References

- `docs/design/2026-10-17-claim-judge-full-source.md`, Phase 0 results
- `docs/design/2026-05-16-extractor-contract-and-l2-summaries.md:592-597`, the origin of the 12K line
- `distillers/src/video.rs`, `distillers/src/session.rs`, `distillers/src/parse.rs`, `distillers/src/validate.rs`, `distillers/src/dispatcher.rs`, `vault/src/distilled.rs`, `vault/src/fabric.rs`
- `borg/src/eval/candidate.rs`, `borg/src/eval/regenerate.rs`, `borg/src/config.rs`, `borg/src/config/harvest.rs`
- Panel round 1 synthesis (earlier design): `~/.cache/review-panel/runs/v2XFzIL6/synthesis.md`
