# Design Document: Distiller Long-Source Claim Fidelity

**Author:** Scott Idler
**Date:** 2026-10-18
**Status:** Draft
**Review Passes Completed:** 4/5 (draft, correctness, clarity, edge cases)

## Summary

Long-source notes carry claims their own source does not back. Two independent judges flag the same original claim on 6 of 29 long-source notes and 1 of 35 short ones. The six were checked by hand, and they trace to three causes: session windowing drops the middle of long sessions, the reduce step rewrites claims without the source, and no pattern carries a claim-honesty rule. Fix the three, and measure on the notes the judges already flagged. This blocks `docs/design/2026-10-17-claim-judge-full-source.md` Phase 0.

## Problem Statement

### Background

- The claim-judge Phase 0 run (2026-10-18, ripr) planted a number swap into notes and asked two judges (claude-sonnet-5, gemini-2.5-pro) to judge the original and the mutant.
  - The swapped claim flipped to not-supported in 25/27 (claude) and 26/27 (gemini) long-source pairs.
  - The paired metric still failed (0.59 / 0.57 against 0.80), because 11-12 of 29 long-source originals were already flagged.
- Scott's call (2026-10-18): fix the distiller first, then re-run that Phase 0. Test the fix on the notes that are already flagged.
- Distill routing today:
  - At or below `SINGLE_CALL_TOKEN_THRESHOLD` (12,000 tokens, `distillers/src/video.rs:38`), one call.
  - Above it, article | video | thread | voicenote run chunk -> map -> reduce, with 8,000-token chunks.
  - Repo is always one call.
  - Session windows the body head+tail to `harvest.token-cap` (default 12,000, `borg/src/config/harvest.rs:167`) before routing, so the session chunk/reduce path only runs when an operator raises the cap (`distillers/src/session.rs:14-17`).

### Problem

The six claims both judges flagged, checked against their staged sources:

| trace | kind | path | error | cause |
|---|---|---|---|---|
| `hv-29b2f05e` | session | windowed single call | "All three security reviews"; the source has 4 review prompts (byte offsets 59, 29,733, 41,685, 47,571) | windowing dropped chars 28,436-30,801, which held the second prompt. Rebuilt window = 11,859 tokens = `meta.input-tokens` exactly |
| `cl-2de2b08b` | video | chunk/reduce | "pulling the center to the backside linebacker"; source at 00:42:14 says "ID linebacker" | reworded at chunk or reduce; chunk outputs are not staged, so which one is unproven |
| `cl-0222aa9e` | video | chunk/reduce | the interviewer's hedged question ("I could be way off on this, so you tell me", 00:58:56) stated as a position | same; the chunk held both question and answer. The judge also never sees `who` (`borg/src/eval.rs:410`) |
| `cl-2db85dab` | repo | single call, full input | "at least nine languages"; the table has 9 repos in 8 languages (zh-CN twice) | no rule on counting distinct items |
| `cl-9e724b4b` | article | single call, full input | merges the 3-Raspberry-Pi "production" cluster (char 193) with the six-node Proxmox/Vultr/OVH cluster (chars 25k-36k) | no rule against cross-section merges |
| `hv-16d375ce` | session | single call, full input | a "Reusable pattern:" lesson the session never states | `distill-session.md:52-53` asks for techniques that generalize |

Three mechanisms, verified in code:

- **Session windowing drops the middle silently to the reader.** `window_head_tail` (`distillers/src/session.rs:614-635`) cuts any body over `token_cap` x 4 chars minus frame overhead (47,420 chars at the default). The model sees a `[TRANSCRIPT TRUNCATED]` marker, but nothing in `meta` records the cut, and `meta.input-tokens` counts the windowed body (`session.rs:176`). Two more of the 15 flagged traces saw 26-32% of their source (`hv-2fe2ec93` 147,632 bytes, `hv-5194d942` 184,203 bytes).
- **The reduce step writes claims without the source.**
  - `build_reduce_input` (`distillers/src/parse.rs:725-748`) renders each pool claim as `[anchor] text`, dropping `kind`, `who` and `quote`.
  - `select_reduce_claims` (`parse.rs:793-826`) builds every final claim from the reduce output, so `kind`/`who`/`quote` are re-derived by a call that saw neither the source nor the chunk metadata.
  - An anchorless claim passes with "NO text-match gate" (`parse.rs:782-785`). An anchored claim is checked on its anchor only, never its text.
  - All five reduce patterns license rewording: "You may lightly consolidate wording, but do not invent facts." (`distill-{article,video,session,thread,voicenote}-reduce.md`).
- **No claim-honesty rule, and no claim-to-source check.** Patterns carry COMMAND-HONESTY and ANCHOR-HONESTY rules, but nothing on counting, merging across sections, hedges, or unstated generalizations. `distillers/src/validate.rs` checks structure only.

### Goals

- Every claim on a long-source note is backed by one passage of the source the model was given. (Scott, 2026-10-18: "fix the distiller's long-source errors first")
- Long sessions are distilled from their whole body, or the note records that they were not.
- The fix is measured on the already-flagged long-source notes, against a same-code baseline. (Scott, 2026-10-18: "pick items that are already problematic")
- The claim-judge Phase 0 re-runs after this ships.

### Non-Goals

- **Excluded: transcript errors.** The judge checks a claim against the staged transcript, so a misheard word ("Grok" for Groq in `ht-37006094`) passes. Neither this doc nor the claim judge can catch it.
- **Excluded: frames and visual content.** None of the six errors involves visuals.
- **Excluded: the Gemini engine's empty first window** (`cl-0222aa9e` starts at `[00:30:00]`). The judges saw the same partial transcript, so it does not explain a flag. It needs its own fix in `borg/src/youtube/gemini.rs`.
- **Parked: sending `who` to the claims judge.** Owned by the claim-judge doc's Parked list. Revisit if `cl-0222aa9e`'s claim still flags after Phase 3 carries `who` through.
- **Parked: a pre-publish grounding check** on top of `quote_grounding` (`borg/src/eval/calc.rs:183`). Revisit if Phase 5 misses its bar.

## Proposed Solution

### Overview

1. Measure first: a throwaway build re-distills the flagged notes on current code, stages each chunk's output, and judges on the full source.
2. Record session windowing in `meta`.
3. Distill long sessions whole through the existing session chunk/reduce path.
4. The reduce step selects pool claims by index and copies them verbatim. It writes no claim text.
5. Add a claim-honesty rule to the single-call and chunk patterns.
6. Re-measure on the same notes and judges.

### Architecture

```
source -> [session: window? (Phase 2: only above a high cap)] -> route
  single call  -> distill-<kind> (Phase 4 rule) -> Distilled
  long         -> chunk xN (Phase 4 rule) -> pool [#i] kind who quote text
               -> reduce selects indexes (Phase 3) -> pool claims verbatim
```

- The pool becomes the only author of reduce claim text. The reduce call keeps writing summary, tldr, key ideas and enumeration.
- Thread reduce has its own input builder, `build_thread_reduce_input`; Phase 3 covers it the same way.

### Data Model

- `meta.validation.bounds-truncations` gains `window:<source_chars>><kept_chars>` when session windowing cut the body. The existing field, no new key.
- Reduce pattern output: each claim is `{ pool: <index> }`. `kind`, `who`, `quote`, `anchor` and `text` come from the pool claim.
- A reduce claim whose index is out of range is dropped and counted in a new `meta.validation.reduce-bad-index` count. Validation never gates publish (`validate.rs:1-5`), so this degrades visibly instead of failing.

### API Design

- No CLI change. `harvest.token-cap` keeps its name; Phase 2 changes its default and meaning (see Open Questions).
- `build_reduce_input` renders pool lines as `[#i] [anchor] (kind, who) text` plus the quote when present.
- `select_reduce_claims(reduce_claims, pool_claims, &mut counters) -> Option<Vec<Claim>>` returns clones of the selected pool claims.

### Implementation Plan

#### Phase 0: Baseline on the flagged notes (zero committed code)
**Model:** opus
- Operator-run on ripr with a throwaway build, the same isolation as the claim-judge Phase 0 (scratch `XDG_DATA_HOME`, scratch patterns dir, live DB row counts checked before and after).
- The build:
  - re-distills named staged traces through the full `Dispatcher` (`borg/src/eval/regenerate.rs:199` is the precedent; the candidate harness cannot, it refuses sources that do not reach a single call, `borg/src/eval/candidate.rs:160`);
  - writes each chunk's raw output and the reduce input next to the run;
  - judges claims on the full source (the claim-judge Phase 0 patch).
- Test set: the 15 flagged traces, `cl-56dd5199 cl-9e724b4b cl-ad229f9f cl-b14d7e33 cl-2db85dab hv-12ae5acc hv-16d375ce hv-29b2f05e hv-2fe2ec93 hv-5194d942 cl-0222aa9e cl-1ed09284 cl-2de2b08b cl-33faf56c cl-38cd120a`. `hv-12ae5acc` and `hv-16d375ce` share one body (same md5), so 14 distinct sources.
- Copy the 15 trace dirs into the Phase 0 scratch tree first. Phases 0 and 5 read the copy, so a `sb borg retention sweep` (manual on ripr, `retention-days: 14`) between them cannot remove the test set.
- Distill each source 3 times on current code. Judge every output with both judges.
- **Success criteria:**
  - A baseline table lands in this doc: per source, both-judge flagged claims per run, claims per note, and whether each of the 6 core errors reproduced.
  - For each run where a video error reproduces, the staged chunk output shows whether the chunk or the reduce wrote it. A video error that never reproduces is recorded as unattributed. The original distill's chunk output was never staged, so the original cannot be attributed.
  - Live `eval_scores` and `judge_trust` row counts are unchanged after the run.

#### Phase 1: Record session windowing
**Model:** sonnet
- `SessionDistiller` pushes `window:<source_chars>><kept_chars>` into `bounds-truncations` when `window_head_tail` cuts.
- **Success criteria:** `cargo test -p distillers session_window_records_bounds_truncation` passes on a 49,759-char body and fails with the push removed.

#### Phase 2: Distill long sessions whole
**Model:** opus
- Raise the default `harvest.token-cap` so bodies above 12,000 tokens route to the existing `distill-session-chunk` / `distill-session-reduce` path. Head+tail windowing applies only above the new cap.
- The new default is Scott's call (Open Questions).
- **Success criteria:** `cargo test -p distillers long_session_reaches_every_prompt` passes: a fixture with 4 review prompts, the second mid-body, sends all 4 to the chunk calls and records no `window:` truncation.

#### Phase 3: Reduce copies pool claims
**Model:** opus
- `build_reduce_input` and `build_thread_reduce_input` index pool lines and carry `kind`, `who` and `quote`.
- The five reduce patterns select by index. Delete the "lightly consolidate wording" line from each, and the merged-claim paragraph from `distill-video-reduce.md:123-125` ("When you consolidate two or more pool claims into one synthesized claim").
- When two pool claims say the same thing, the reduce picks one index. It never writes a merged claim.
- `select_reduce_claims` returns pool claims verbatim and counts bad indexes.
- **Success criteria:**
  - `cargo test -p distillers reduce_claims_are_pool_claims_verbatim` passes: a reduce reply with altered text still yields the pool text, `who` and `kind`.
  - `rg -l "consolidate wording" borg/patterns | wc -l` prints `0`.

#### Phase 4: Claim-honesty rule
**Model:** opus
- One CLAIM-HONESTY block, the same text in every single-call and chunk distill pattern:
  - count distinct items, not rows or mentions;
  - state no total over a truncated view;
  - a question or a hedge is not a position;
  - one claim, one passage: never join facts from separate sections;
  - no lesson or generalization the source does not state.
- Narrow `distill-session.md:52-53` so a "reusable pattern" must be stated in the session.
- Score the single-call patterns with `sb borg eval --candidate-pattern` on short traces, so the rule does not cost coverage (`eval.candidate.max-coverage-cut`).
- **Success criteria:** each changed single-call pattern's candidate run reports no coverage cut beyond `max-coverage-cut`.

#### Phase 5: Re-measure (operator, ripr)
**Model:** opus
- The Phase 0 build and procedure, rebased on the shipped code.
- **Success criteria:**
  - None of the 6 core errors reproduce in any of 3 runs.
  - Both-judge flagged claims on the 14 sources are below the Phase 0 baseline.
  - Claims per note do not drop more than `eval.candidate.max-coverage-cut` against the baseline.

## Acceptance Criteria

- [ ] No reduce pattern licenses rewording: `rg -l "consolidate wording" borg/patterns | wc -l` prints `0`.
  - Observed on main: `5`.
- [ ] Reduce claims are pool claims: `cargo test -p distillers reduce_claims_are_pool_claims_verbatim` passes.
  - Cannot run yet: the test lands in Phase 3.
- [ ] A long session reaches the model whole: `cargo test -p distillers long_session_reaches_every_prompt` passes.
  - Cannot run yet: the test lands in Phase 2.
- [ ] Phase 5's table shows 0 of 6 core errors reproduced and fewer both-judge flags than Phase 0.
  - Cannot run yet: needs Phases 0-4.

## Resolved Decisions

- 2026-10-18 (Scott): fix the distiller before the claim-judge Phase 0 re-run, and test on the already-flagged notes.

## Alternatives Considered

### Alternative 1: Gate reduce text drift instead of copying
- **Description:** keep reduce-written text, reject a claim whose text drifts from its cited pool line past a similarity bar.
- **Pros:** keeps cross-chunk consolidation.
- **Cons:** a similarity bar is a tunable that passes "ID" -> "backside" (one word). Consolidation is what produced merges.
- **Why not chosen:** copying is the direct mechanism and has no threshold to tune.

### Alternative 2: Keep head+tail windowing, forbid totals in the session pattern
- **Description:** leave `token-cap` at 12,000 and add a rule against counting across the marker.
- **Pros:** no cost change.
- **Cons:** `hv-5194d942` keeps 26% of its source. A rule cannot recover what the model never saw.
- **Why not chosen:** it fixes one symptom and leaves the coverage gap.

### Alternative 3: Pre-publish claim grounding check
- **Description:** score each claim against the source with shingles before publish.
- **Pros:** catches errors from any path.
- **Cons:** `quote_grounding` works on quotes; most claims carry none, and paraphrase grounding has no measured precision.
- **Why not chosen:** parked (Non-Goals) until Phase 5 shows the cause fixes are not enough.

## Technical Considerations

### Dependencies
- The claim-judge Phase 0 patch (scratchpad, not committed) for full-source judging in Phases 0 and 5.
- Blocks the claim-judge doc's Phase 0 re-run. Single repo; no cross-repo blast radius.

### Performance
- Phase 2 turns a long session from one call into N chunk calls plus one reduce. `hv-5194d942` (184k chars) goes from 1 call to about 7.
- Phase 3 shortens the reduce output (indexes, not text).

### Security
- No new inputs, credentials or network surface.

### Testing Strategy
- Unit tests per phase, each shown to fail with its change removed.
- Phases 0 and 5 are the end-to-end measure, 3 runs per source, because LLM output varies run to run.

### Rollout Plan
- `bump` + `otto deploy`. New ingests take the new path. Old notes change only on `sb borg replay`.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Verbatim pool claims read choppier or duplicate across chunks | Med | Low | reduce still picks which claims; duplicates are a selection rule in the pattern |
| The honesty rule cuts claims | Med | Med | Phase 4 candidate runs gate on `max-coverage-cut` |
| 3 runs is too few to separate fix from noise | Med | Med | the bar is reproduction of named errors, plus a count against a same-code baseline |
| Session chunk/reduce path has rotted (it only runs above the cap) | Low | Med | Phase 2's test drives it end to end |
| Judges vary run to run too, so a flag count moves on its own | Med | Med | both runs (Phases 0 and 5) use the same judges, models and full-source build; the 6 core errors are checked by hand, not only by count |
| A copied pool claim keeps a chunk-level error (the `cl-2de2b08b` word swap if Phase 0 shows the chunk wrote it) | Med | Med | Phase 4's rule lands in the chunk patterns too |

## Open Questions

- [ ] Phase 2: what is the new `harvest.token-cap` default? The largest over-window session seen is `hv-fef7fae9` at 614k chars (about 154k tokens). Windowing above the new cap stays.

## References

- `docs/design/2026-10-17-claim-judge-full-source.md`, Phase 0 results
- `distillers/src/session.rs`, `distillers/src/parse.rs`, `distillers/src/validate.rs`
- `borg/patterns/distill-*-reduce.md`, `borg/patterns/distill-session.md`
- `borg/src/eval/candidate.rs`, `borg/src/eval/regenerate.rs`
