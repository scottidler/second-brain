# Design Document: Claim Judging Over the Full Source

**Author:** Scott Idler
**Date:** 2026-10-17
**Status:** Approved for build (2026-10-18, Scott: option B). Phase 0 is closed by its claim-level numbers; no re-run. Phases 1-3 as written.
**Review Passes Completed:** 3/5 (draft, correctness, edge cases); panel round 1 folded (staff seat only)

## Summary

`judge-claims` sees only the first ~24k chars of a source and its rubric tells it to fail anything past the cut. So every note with a long source fails, and Claude's paired catch is stuck at 0.69. The 24k cap was never a model limit. Give the claims judge the whole source up to a configured cap (default 400k chars), mark sources over that cap `not judgeable` instead of failing, make `--inject --sample N` mean N pairs per class, and re-measure trust.

## Problem Statement

### Background

- The automated note quality loop (`docs/design/2026-10-14-automated-note-quality-loop.md`) shipped in v0.18.0. Remediation and GEPA prompt candidates are gated on judge trust: paired catch >= 0.80 on >= 30 applicable traces (`borg/src/config/eval.rs` `TrustConfig`, `borg/src/eval/trust.rs:181-247`).
- No LLM judge check is trusted today, so both gated features ship switched off.
- Live `judge_trust` rows for `judge-claims` (rubric `v2`, `inject-20261017T150659Z`):
  - `claude-sonnet-5`, number-swap: n 26, paired catch 0.69
  - `gemini-2.5-pro`, number-swap: n 26, paired catch 0.58
- Handoff with the full findings: `docs/handoff/claim-judge-full-source.md`.

### Problem

The low trust comes from truncation, not from the judge being bad at the job.

- **The cut:** `judged_input` (`borg/src/eval.rs:561-572`) trims the source to `source_budget(JUDGE_INPUT_MAX_CHARS = 24_000)` (`borg/src/eval/judge.rs:106,120-122`). Both judges get the trimmed source.
- **The rubric rule:** `borg/patterns/judge-claims.md:21-22`: "If the SOURCE appears cut off and a claim plausibly comes from the missing part, answer unsupported." Every claim from past the cut is failed by instruction.
- **Why claims come from past the cut:** the distiller covers the whole source. It chunks long sources at 32k chars (`distillers/src/video.rs:37,40,661`) and reduces, so its claims come from anywhere in the source.
- **Measured on the 30-trace inject sample:**
  - Sources <= 24k: 173 claims, 2 flagged by both judges (1.2%).
  - Sources > 24k: 66 claims, 36 flagged by both (55%).
  - Agents re-checked all 38 jointly flagged claims against the full source: 29 supported, 9 partial, 0 unsupported, 0 contradicted.
  - Claude paired catch: 0.90 on fitting sources (n 20), 0.00 on oversized (n 6). Originals flagged: 0.10 vs 1.00.
- **How much it affects:** 1,565 of 4,178 staged traces (37%) have a source over the budget. Over-window sources: median 45.9k chars, p90 298k, max 2.47M.
- **The 24k is arbitrary:** it dates to `f1572c5` (2026-07-05) and was chosen as "larger than oracle's 8k". It does not come from a model's context limit.

### Goals

- `judge-claims` judges every claim against the full source whenever the source is under a configured cap. (Scott, 2026-10-17)
- A source over the cap is `not judgeable`. It never counts as failing and carries no verdicts. It counts as covered, so the backlog does not re-queue it. It is excluded from trust math. (Handoff, "Interim rule")
- `sb borg eval --inject --sample N` yields >= N applicable pairs for every (check, class) it can measure. (Handoff, "Re-measure trust")
- `CLAIMS_RUBRIC_VERSION` bumps, so `v2` trust rows stop counting. (Handoff)
- Re-measured: `claude-sonnet-5` `judge-claims` number-swap paired catch >= 0.80 on >= 30 pairs, with the over-window pairs reported separately. (Scott, 2026-10-17)

### Non-Goals

- **Excluded:** `judge-distillation`. Its rubric has no claim-accuracy axis (paired 0.07-0.65), so a longer source does not fix it. It keeps the 24k `JUDGE_INPUT_MAX_CHARS`.
- **Excluded:** passing quotes through `build_reduce_input` (`distillers/src/parse.rs:725-749`). That changes distill output, so it belongs in a distiller doc.
- **Parked:** oracle answerability (`SOURCE_TEXT_MAX_CHARS = 22_000`, `oracle/src/eval/answerability.rs:30`; 297/579 truncated). Revisit if this design passes trust: the same cap change applies there, but its evidence is per question.
- **Parked:** adding `who` to the claims input, which would make who-change applicable to `judge-claims`. Revisit once number-swap is trusted.
- **Parked:** the per-claim evidence locator (Alternative 2). Revisit if Phase 0 fails its go rule.

## Proposed Solution

### Overview

The simplest change that removes the measured cause: stop cutting the source for the claims judge.

- The claims judge gets its own limit, `eval.claims-source-max-chars` in `borg.yml`, default 400,000. That covers the over-window p90 (298k).
- A source at or under that limit goes to the judge whole, with no trimming at any layer.
- A source over the limit gets no fabric call. Its `judge-claims` row is `{"applicable":false,"reason":"source-over-cap"}`, which is the house `not_applicable` shape (`borg/src/eval/scores.rs:320`).
- The rubric line telling the judge to fail cut-off claims is removed. The judge never sees a cut source now.
- Phase 0 proves, before any code, that both judges keep their accuracy on long inputs.

### Architecture

```
Fixture (kind, distilled, full source)
  -> judgeability: source chars <= eval.claims-source-max-chars ?
       no  -> row {"applicable":false,"reason":"source-over-cap"}, zero fabric calls
       yes -> claims_input(kind, claims, FULL source)
              -> fabric -p judge-claims, max_chars = 0 (no truncation), timeout = eval.claims-timeout-secs
              -> parse_claim_verdicts (unchanged)
```

- Judgeability depends on **source length only**, never on note or claims length. A baseline and its candidate re-distill of the same trace always land on the same side.
- One gate, every caller: `--staged`, `--unscored`, `--inject`, and the `--remediate` before-score all reach `judge_claims` through the same gate. No path keeps the 24k trim for claims.
- `judge-distillation` keeps `judged_input` and `JUDGE_INPUT_MAX_CHARS` exactly as they are.

### Data Model

- `borg/src/config/eval.rs`, `EvalConfig`, two new kebab-case keys with defaults:
  - `claims-source-max-chars: usize`, default `400_000`.
  - `claims-timeout-secs: u64`, default set from Phase 0's uncensored latency: `max(90, 2 * p99)` rounded up to a multiple of 30s, and never below Phase 0's max.
  - Both reach the judge the way existing eval settings do: `eval_opts` (`sb/src/cli/borg.rs:671`) -> `EvalOpts` (`borg/src/eval.rs:73`) -> `fabric_judges` (`borg/src/eval.rs:214`).
- `borg/src/eval/cache.rs:23`: `CLAIMS_RUBRIC_VERSION` `v2` -> `v3`.
- `eval_scores` for `check_name='judge-claims'`:
  - Judged: `{"verdicts":[...]}` as today.
  - Not judgeable: `{"applicable":false,"reason":"source-over-cap","source_chars":N,"cap":C}`, `error` null.
  - **A cap change re-opens coverage.** `is_covered` (`scores.rs:270-297`) treats a `source-over-cap` row as not current when its `source_chars` is at or under the configured cap. Raising the cap re-queues exactly the traces that now fit, and nothing else.
    - Signature: `is_covered(&self, trace, content_hash, required, claims_cap: usize)`. `ScoreStore` holds only a connection, so the cap is a parameter, not store state.
    - Predicate, added to the coverage query: `AND NOT (json_extract(metrics_json, '$.reason') = 'source-over-cap' AND json_extract(metrics_json, '$.source_chars') <= ?4)`. JSON1 ships with rusqlite's `bundled` feature (`Cargo.toml:47`).
    - Both callers get the cap from `EvalOpts.claims_source_max_chars`: `backlog::find_backlog` (`backlog.rs:53`, call at `:82`, reached from `sb/src/cli/borg.rs:539`) and `weekly::remediated::collect` (`remediated.rs:81`, call at `:201`, reached from `sb/src/cli/borg/weekly.rs`). Each takes a new `claims_cap: usize` argument.
  - `content_hash` stays the distill hash (`eval.rs:565`). Remediate and backlog key on it (`remediate.rs:221`, `backlog.rs:80`).
- Claims cache key: `stable_hash(kind \n full source \n claims)`, plus fixture id, model and rubric. Same shape as `eval.rs:411`, but over the full source.
- In memory: `TraceRecord.metrics.judge_claims` (`eval.rs:447`) changes from `Option<Vec<ClaimVerdict>>` to a three-state enum:

```rust
pub enum ClaimsOutcome {
    Judged(Vec<ClaimVerdict>),
    NotJudgeable { source_chars: usize, cap: usize },
    NotRun,
}
```

  `NotRun` is today's `None`. The name says why each case has no verdicts. `NotJudgeable` carries the cap that applied, because `rows_from_record` (`scores.rs:375`) builds the stored row and the weekly text from the record alone.

- JSONL shape (`TraceMetrics`, serialized at `report.rs:207`), via a custom serialize:
  - `Judged` keeps today's array of verdict strings, so `tests.rs:765` and the `uv` contract are unchanged.
  - `NotJudgeable` serializes as `{"applicable":false,"reason":"source-over-cap","source_chars":N,"cap":C}`.
  - `NotRun` serializes as `null`.
- Consumers, each with a decided behavior:
  - **`rows_from_record`** (`scores.rs:375-391`): writes the row shapes above.
  - **`inject::flags`** (`inject.rs:240-252`): `NotJudgeable` -> excluded (see API Design).
  - **`candidate::NoteCounts::of`** (`candidate.rs:209`) and the sample-complete check (`candidate.rs:827`): over-cap traces are removed from the candidate sample before scoring. The filter runs at the top of `candidate::score` (`candidate.rs:659`), over the loaded fixtures, before any distill call. Baseline and candidate share a source, so both sides drop together. The sample is not marked incomplete. The report prints how many were dropped.
  - **`candidate::feedback_for`** (`candidate.rs:856`): `NotJudgeable` gives no claim feedback, and says so in the feedback text.
  - **`agreement::trace_flag` / `claim_support`** (`agreement.rs:158,168-176`): these read stored JSON, not the enum. No type change. A test proves `applicable:false` rows are not raters.
  - **Weekly remediated report** (`weekly/remediated.rs:218-232`): a trace whose only non-failing required check was `not judgeable` reports `Outcome::NotJudgeable` ("judge-claims not judgeable: source N chars > cap C"), never `Passed`.

### API Design

- `borg/src/eval/judge.rs`: `claims_input(kind, claims, source) -> String` (today's `judge.rs:351-358` format, unchanged).
- `ClaimJudge::judge_claims` gets the full source, and `FabricClaimJudge` passes `max_chars = 0` to `vault::fabric::run_command`. Fail closed: the caller checks judgeability first, and the judge returns an error if handed a source over the cap. It never truncates silently.
- `borg/src/eval/inject.rs`:
  - `ClassTally` gets a not-judgeable path. Excluded pairs count neither toward `n` nor as errors (`inject.rs:271-275`).
  - The report prints, per (model, check, class): pairs `fits | over-window | over-cap`, with paired catch for the first two.
  - "over-window" means over the old 24k budget. It stays reported so the long-source result is visible on its own line.
- `sample_fixtures(staged_root, sample)` (`inject.rs:457-472`): adds traces from a deterministic, kind-stratified order until every (check, class) pair that `applies` (`inject.rs:92-99`) has >= `sample` plantable, judgeable traces. It stops when the eligible pool runs out and says which class fell short. The `--sample` help text changes to say this.
- `borg/patterns/judge-claims.md`: delete lines 21-22, the cut-off rule. The verdict set is unchanged.

### Implementation Plan

#### Phase 0: Prove long-context judging holds (zero committed code)
**Model:** opus (operator run on ripr; makes paid calls)
- **Isolation: Phase 0 never touches the live eval DB.**
  - `run_eval_inject` opens `vault::paths::borg_eval_cache_path()` (`sb/src/cli/borg.rs:600`). That path follows `XDG_DATA_HOME` (`vault/src/paths.rs:48-56`).
  - Run with `XDG_DATA_HOME=<scratchpad>/xdg`, holding a copy of `eval-cache.db`. The path must be absolute: `xdg_data_dir` silently ignores a relative value and falls back to the live `~/.local/share` (`vault/src/paths.rs:48-56`).
  - Use a scratch `borg.yml` with an absolute `staging.root` pointing at the live stages, because the default staging path also follows `XDG_DATA_HOME`.
  - The edited pattern goes in a scratch dir passed as `--patterns-dir` (accepted with `--inject`, `sb/src/cli/borg/args.rs:299-303`).
- **Throwaway local build, never committed:**
  - `judge_claims` gets the full source with `max_chars = 0`.
  - The claims timeout is 600s, so slow calls are measured instead of cut off.
  - The planted number is recorded per pair, so the bucket rule below can be applied.
- **Sample:**
  - The natural `--inject --sample 60` run.
  - Plus every over-window number-swap pair available, until >= 15 qualify under the bucket rule.
  - Plus >= 5 deliberately chosen sources between 250k and 400k chars.
- **Over-window bucket rule:** a pair counts as over-window only if the mutated claim's ORIGINAL number first occurs in the source past char 24,000. A long source whose evidence sits in the head is not evidence for this design.
- **Record per pair:**
  - source chars and per-call latency
  - the mutated claim's own verdict on original and mutant
  - the pair outcome per judge, plus errors (timeouts, parse failures) as attempted vs completed
- Write the results into this doc under Phase 0. Spike scripts stay in the session scratchpad, never under `docs/`.
- **Go rule**, for each of `claude-sonnet-5` and `gemini-2.5-pro`:
  - Over-window bucket: paired catch >= 0.80 on >= 15 completed pairs.
  - The mutated claim itself is `contradicted` or `unsupported` on >= 0.80 of caught mutants.
  - Errors <= 10% of attempted pairs. An errored pair counts as a miss for the go rule.
  - Fitting-source paired catch >= 0.85.
  - The 250-400k set: zero errors and paired catch reported.
  - If one judge passes and the other fails, the design still ships. The failing judge stays untrusted, which the trust rule already handles per model.
  - If both fail: no-go.
- **No-go:** the doc returns to Draft with Alternative 2 (evidence locator) as the design. Phases 1-3 do not start.
- **Success criteria:**
  - The Phase 0 section of this doc carries the per-bucket table (n, attempted, errors, paired catch, mutated-claim verdict rate) per model.
  - It also records p50/p99/max latency per model and the 250-400k results.
  - The go rule is evaluated in writing with the numbers.
  - `sqlite3 -readonly ~/.local/share/sb/borg/eval-cache.db "select count(*) from judge_trust where measured_at > '<phase-0 start>'"` returns 0.
  - `sqlite3 -readonly ~/.local/share/sb/borg/eval-cache.db "select count(*) from eval_scores where scored_at > '<phase-0 start>'"` returns 0. `run_inject` writes `eval_scores` rows too (`inject.rs:566-570`), not only trust rows, and an unclassed record keeps its baseline `run_kind`, so the check counts every kind. Run it with the nightly `sb-eval` timer paused, or it counts that run's rows.

##### Phase 0 results (2026-10-18, ripr)

- **Run:** the throwaway build with full source, no input cap and a 600s claims timeout. The claims cache key was salted, so every claims verdict was a fresh call. `--inject --sample 60` plus 30 over-window and 6 large extras, 8 workers, the pattern with the cut-off rule deleted.
- **Pairs:** 332 planted, of which 82 number-swap. Claims calls: 540, across two logs (a first launch at 4 workers was stopped after 12 calls and relaunched; its cached verdicts are merged).
- **Isolation held:** after the run, the live `eval-cache.db` reads `eval_scores` 4133 and `judge_trust` 11, the same as before. 0 rows of either have a timestamp after the 2026-10-18T01:00:39Z start. The scratch copy holds the run's 3843 score rows. `sb-eval.timer` was paused for the run and restarted after.

Number-swap pairs, paired catch as the go rule defines it (the mutant is flagged and the original is not):

| model | bucket | attempted | errors | paired catch | errors as miss | mutated claim contradicted/unsupported, of caught |
|---|---|---|---|---|---|---|
| claude-sonnet-5 | fits (source <= 24k) | 35 | 0 | 0.86 (30/35) | 0.86 | 1.00 |
| claude-sonnet-5 | over-window | 30 | 1 | 0.59 (17/29) | 0.57 | 1.00 |
| claude-sonnet-5 | long, number in head | 17 | 1 | 0.50 (8/16) | 0.47 | 1.00 |
| claude-sonnet-5 | 250-400k | 7 | 1 | 0.33 (2/6) | 0.29 | 1.00 |
| gemini-2.5-pro | fits | 35 | 0 | 0.69 (24/35) | 0.69 | 1.00 |
| gemini-2.5-pro | over-window | 30 | 0 | 0.57 (17/30) | 0.57 | 1.00 |
| gemini-2.5-pro | long, number in head | 17 | 0 | 0.53 (9/17) | 0.53 | 1.00 |
| gemini-2.5-pro | 250-400k | 7 | 0 | 0.57 (4/7) | 0.57 | 1.00 |

Claims-call latency in seconds, successful calls only:

| model | n | p50 | p99 | max | p50 on >250k sources | max on >250k sources |
|---|---|---|---|---|---|---|
| claude-sonnet-5 | 263 | 5.9 | 34.6 | 39.5 | 4.4 | 34.6 |
| gemini-2.5-pro | 269 | 16.8 | 41.9 | 55.3 | 29.0 | 55.3 |

- **Errors:** claude-sonnet-5 had 8 errored claims calls, all `fabric ... failed: empty response`, on sources of 22,581, 80,541 (4 calls) and 313,743 chars (3 calls). gemini-2.5-pro had none. No call hit the timeout.
- **Why pairs miss.** At the claim level, the judges almost never miss the swap. On over-window pairs, the swapped claim is supported on the original and not supported on the mutant for 25/27 (claude-sonnet-5) and 26/27 (gemini-2.5-pro). The paired misses come from the original note already being flagged by some other claim:

| bucket | originals flagged, claude-sonnet-5 | originals flagged, gemini-2.5-pro | both judges flag the same original claim |
|---|---|---|---|
| fits | 5/35 | 11/35 | 1/35 |
| over-window | 11/29 | 12/29 | 6/29 |
| long, number in head | 6/16 | 8/17 | 3/16 |

  The two judges independently flag the same original claim on 6 of 29 over-window notes, against 1 of 35 fitting notes. Long-source notes carry more claims that their own source does not back. Those look like defects in the distillation, not judge noise. The paired metric counts each one as a miss.
- **Go rule, as written:**
  - claude-sonnet-5: over-window 0.59 < 0.80, FAIL. Mutated-claim rate 1.00, pass. Errors 3/82 pairs, pass. Fitting 0.86 >= 0.85, pass. 250-400k set: 1 error, FAIL (the rule asks for zero).
  - gemini-2.5-pro: over-window 0.57 < 0.80, FAIL. Fitting 0.69 < 0.85, FAIL. Mutated-claim rate 1.00, pass. Errors 0, pass. 250-400k set: 0 errors, paired catch 0.57.
  - **Both judges fail, so the written rule says no-go.** The claim-level data says the failure is not long-context reading. Alternative 2 (the evidence locator) does not change whether the original note is flagged, so the no-go branch as written would not move these numbers.
- **Decision (Scott, 2026-10-18): fix the distiller first, then re-run Phase 0.** The six both-flagged claims were checked against their sources. Each one goes beyond its source: a miscount (9 repos read as 9 languages, `cl-2db85dab`), two clusters merged into one (`cl-9e724b4b`), a word swap (ID linebacker -> backside linebacker, `cl-2de2b08b`), a hedged question stated as a position (`cl-0222aa9e`), an invented count of three reviews (`hv-29b2f05e`), and a lesson the session never states (`hv-16d375ce`). Truncation is ruled out for the article, repo and videos only. `meta.input-tokens` counts what the model was sent, not the source. Session head+tail windowing (`distillers/src/session.rs:168-175`) dropped chars 28,436-30,801 of `hv-29b2f05e`, which held 1 of its 4 security-review prompts, so the model saw three. `distillers/src/validate.rs` checks structure only, so no step compares a claim to its source before publish. The distiller fix is tested on the notes already flagged: the 15 over-window originals that either judge flagged (`cl-56dd5199 cl-9e724b4b cl-ad229f9f cl-b14d7e33 cl-2db85dab hv-12ae5acc hv-16d375ce hv-29b2f05e hv-2fe2ec93 hv-5194d942 cl-0222aa9e cl-1ed09284 cl-2de2b08b cl-33faf56c cl-38cd120a`), with the six both-flagged ones as the core.

- **Follow-up (2026-10-18, Scott: option B).** No re-run. The paired metric now reads the planted claim (see Resolved Decisions), and at that unit this run's over-window numbers are 25/27 and 26/27. Phases 1-3 proceed as written; Phase 3's `--inject` on ripr is the re-measure.

#### Phase 1: Full-source claims judging + not judgeable
**Model:** opus
- Config keys `claims-source-max-chars` and `claims-timeout-secs` (Data Model), threaded through `eval_opts` -> `EvalOpts` -> `fabric_judges`, and added to the shipped `borg.yml` template with comments.
- The `ClaimsOutcome` enum replaces `Option<Vec<ClaimVerdict>>`. Every consumer listed in Data Model gets its decided behavior, including the JSONL shape.
- Judgeability gate before the fabric call. Not-judgeable rows are written as in Data Model. The cap-change rule goes into `is_covered`.
- Delete the cut-off rule from the pattern. Bump `CLAIMS_RUBRIC_VERSION` to `v3`.
- **Success criteria:**
  - Unit test `judge_claims_over_cap_is_not_judgeable`:
    - A 500k-char source with the default cap makes `MockClaimJudge::calls() == 0` and writes `applicable:false` with `source_chars: 500000`.
    - `is_covered` is true for that trace, and false once the cap is raised to 600k.
    - Break the gate and the test fails.
  - Unit test: a 300k-char source reaches the judge with all 300k chars (the mock records the input length). Re-introduce a trim and the test fails.
  - Unit test: a `borg.yml` with `claims-source-max-chars: 1000` and `claims-timeout-secs: 45` yields `EvalOpts`/`FabricClaimJudge` carrying 1000 and 45.
  - Unit tests: weekly reports `NotJudgeable`, not `Passed`. The candidate drops over-cap traces and stays complete. JSONL round-trips all three `ClaimsOutcome` shapes.
  - `rg -n 'appears cut off' borg/patterns/judge-claims.md` returns no lines, and `rg -n 'CLAIMS_RUBRIC_VERSION: &str = "v3"' borg/src/eval/cache.rs` returns one line.

#### Phase 2: Per-class sampling + bucketed inject report
**Model:** sonnet
- The `sample_fixtures` growth rule, the `ClassTally` not-judgeable path, and the bucketed report (API Design).
- **Success criteria:**
  - Unit test: a fixture pool where only 1 in 3 traces has a number, run with `sample = 30`, yields >= 30 number-swap pairs.
  - Unit test: a pool too small reports the short class by name and does not panic.
  - Unit test: a not-judgeable pair is in neither `n` nor `errors` of its `ClassTally`. Remove the path and the test fails.

#### Phase 3: Ship and re-measure
**Model:** sonnet
- Ship per the release rulings (`bump release`, `otto deploy`). This repo ships direct to main.
- Run `~/.cargo/bin/sb borg eval --inject --sample 30` on ripr. Paste the report into the implementation notes.
- **Success criteria:**
  - `judge_trust` has a `v3` `judge-claims` number-swap row for `claude-sonnet-5` with `sample_size >= 30` and `paired_catch_rate >= 0.80`.
  - The inject report's over-window line for that row shows n >= 10 and paired catch >= 0.80. This is an acceptance bar, not a new code gate.
  - `~/.cargo/bin/sb borg eval --remediate` prints no `judge-claims` trust failure for `claude-sonnet-5`.
  - If any of these fail, the work is not done. Record the report verbatim and go back to the design. Do not re-run hoping for a pass.

## Acceptance Criteria

- [ ] `sqlite3 -readonly ~/.local/share/sb/borg/eval-cache.db "select judge_model, sample_size, round(paired_catch_rate,2) from judge_trust where check_name='judge-claims' and planted_class='number-swap' and rubric_version='v3'"` returns a `claude-sonnet-5` row with `sample_size >= 30` and `paired_catch_rate >= 0.80`.
  - Observed on main (2026-10-17): no `v3` rows. The `v2` rows are `gemini-2.5-pro|26|0.58` and `claude-sonnet-5|26|0.69`.
- [ ] `rg -n 'appears cut off' borg/patterns/judge-claims.md` returns nothing.
  - Observed on main: `21:If the SOURCE appears cut off and a claim plausibly comes from the missing part,`
- [ ] `rg -c 'claims-source-max-chars' borg/src/config/eval.rs` returns >= 1.
  - Observed on main: no match (exit 1).
- [ ] The `--inject` report prints a `fits | over-window | over-cap` pair count for `judge-claims` number-swap, with over-window n >= 1.
  - Observed on main: no bucketed output. `--sample` help reads "plant errors into N traces" (`sb borg eval --help`, line 25).
- [ ] Unit test `judge_claims_over_cap_is_not_judgeable` passes, and fails when the gate is removed.
  - Cannot run on main: written in Phase 1.

## Resolved Decisions

- 2026-10-18 (Scott, option B): build Phases 1-3 now; no Phase 0 re-run and no distiller fix first. Reason: the paired metric was the problem. Trust now scores the planted claim (`2026-10-14-automated-note-quality-loop.md`, Resolved Decisions, 2026-10-18), and on that unit Phase 0 already reads 25/27 (claude) and 26/27 (gemini) over-window against the 0.80 bar. The distiller design (`2026-10-18-distiller-long-source-claim-fidelity.md`) is closed as not pursued. Phase 3's bar is unchanged in words and now measures the planted claim.
- 2026-10-18 (Phase 0), superseded the same day by the bullet above: fix the distiller's long-source claim errors first, then re-run Phase 0. Reason: the judges catch the planted swap (25/27 and 26/27 over-window), and the paired metric fails on originals the distiller already got wrong. The fix's test set is the already-flagged originals.
- 2026-10-17: Full source over evidence windows (Alternative 2), gated by Phase 0. Reason: it is the direct fix for the measured cause and needs no retrieval layer. The locator stays parked with a revisit condition.
- 2026-10-17: Judgeability is keyed on source length alone, so a baseline and its candidate never split across the line.
- 2026-10-17: Fitting sources keep whole-source judging. Nothing changes for them except the rubric version and the deleted cut-off line, which they never triggered.
- 2026-10-17: `judge-distillation` is out of scope. Its weakness is its rubric, not the cut.
- 2026-10-17: No new verdict (`not-located` etc.). With the whole source in view, "the source does not say it" is a true `unsupported`.
- 2026-10-18 (panel round 3, architect seat): 2 must-fixes and 3 cheap wins folded in. `is_covered` gains a `claims_cap` argument and a JSON1 predicate, threaded from both callers. `NotJudgeable` carries its cap. Phase 0 proves zero inject `eval_scores` rows too, and requires an absolute `XDG_DATA_HOME`. The candidate filter's location is named. Dropped: the architect's N+1 concern on `is_covered` (one indexed query per trace, as today).
- 2026-10-17 (panel round 1, staff seat): all 7 must-fixes and 4 cheap wins folded in. Phase 0 isolation, the bucket rule, the per-judge go rule with an error ceiling, the uncensored timeout, every consumer of the outcome, the cap-change re-coverage, config threading, and a Phase 3 bar that fails when trust fails.
- 2026-10-17 (panel round 1, deferred with reason):
  - The cost upper bound: no observed problem, and `eval.limit` caps daily load.
  - Certifying the fabric binary: Phase 0's live calls answer it.
  - `eval_scores.content_hash` ignoring source changes past 24k: staged sources are not rewritten after capture.
- Panel round 1 had one seat: the architect (Gemini) timed out (rc=124), with its reads denied by policy.

## Alternatives Considered

### Alternative 1: Not judgeable only (no fix)
- **Description:** Mark every over-24k source `not judgeable` and stop there.
- **Pros:** No paid spike. Trust likely passes on fitting sources (Claude 0.90, n 20).
- **Cons:** 37% of traces never get claim-checked. The traces most likely to need remediation (long, map-reduced) are exactly the ones skipped.
- **Why not chosen:** It gives up the check's value on the hard cases. It survives inside the design as the over-cap rule.

### Alternative 2: Per-claim evidence locator
- **Description:** Find each claim's passage and send the judge only those excerpts under the 24k budget. Order of lookup: video anchor -> transcript stamp, then grounded quote -> shingle offset, then BM25 top-k over overlapping windows (`vault::embedding::chunk_transcript`).
- **Pros:** Small, fixed input. Excerpts could be shown in the weekly report.
- **Cons, measured read-only over 4,178 traces:**
  - Deterministic location covers only 0.69 of over-window claims overall: article 0.24, repo 0.39, session 0.42. Reduce-path claims mostly have no quote (articles 0/199) because `build_reduce_input` drops them.
  - BM25 fallback top-1 is 0.70 (session 0.57). A retrieval miss on a true claim reads as `unsupported`, which is the same false flag this design exists to kill.
  - 10 claims x top-3 x 2k = 60k chars, which does not fit 24k, so it needs multiple calls or a bigger budget anyway.
- **Why not chosen:** More machinery, and a known false-flag floor. Parked: it becomes the design if Phase 0 fails.

### Alternative 3: Judge per distiller chunk
- **Description:** Re-chunk the source the way the distiller did, and judge claims against chunks.
- **Cons:** Chunk boundaries are not persisted (staged dir census). The reduce step drops a claim's chunk of origin. Each 32k chunk is over the 24k budget.
- **Why not chosen:** There is no way to know which chunk a claim came from.

### Alternative 4: Raise `JUDGE_INPUT_MAX_CHARS` for both judges
- **Description:** One constant, one change.
- **Why not chosen:** It changes `judge-distillation` scores and rubric for no measured gain, which is out of scope. A claims-only key also names what it governs.

## Technical Considerations

### Dependencies
- No new crates. Config only through the existing `EvalConfig`.
- Vendors: `gemini-2.5-pro` and `claude-sonnet-5` via fabric. This assumes both accept a ~100k-token input. Phase 0 proves it on live calls rather than taking it from memory.

### Performance
- Cost: one claims call per trace per model, as today. Input grows from <= 24k chars to the source size.
  - Phase 0 records actual input sizes and latency.
  - Daily load is capped by `eval.limit` (100 traces).
- Latency: today's 90s timeout may be too short for 400k-char inputs, so `claims-timeout-secs` is set from Phase 0's p99.
- `--unscored` stays sequential per model (`backlog.rs:145-204`).

### Security
- No change. The source is the same captured text the distiller already sent to an LLM. The pattern's prompt-injection line stays.

### Testing Strategy
- Unit tests per phase criteria, each proven to bite (break -> fail -> restore).
- Phase 0 and Phase 3 are live measurements on ripr, with numbers pasted into the doc and the implementation notes.

### Rollout Plan
- Phase 0 -> go/no-go -> Phases 1-2 (per-phase commit, `otto ci` green) -> implementation audit -> ship -> Phase 3 re-measure.
- The rubric bump makes every trace owe a new `judge-claims` row (`scores.rs:88-107`). The daily timer works the backlog at `eval.limit` (100) per run, oldest first (`backlog.rs:25,66`).
- Errors on retry: an error row is not covered, so the trace is picked again the next day. A trace that errors every day would hold a slot each run.
  - The source-length gate removes the main cause before the call: over-cap sources never call the judge.
  - The Phase 0 timeout sizing removes the other cause.
  - A per-trace retry ceiling is parked: revisit if `sb doctor` shows the same trace erroring three runs in a row.
- Remediation and GEPA turn on only when trust passes. That is the existing gate, untouched.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Judges lose accuracy deep in long inputs | Med | High | Phase 0 go rule on over-window pairs, before any code |
| A natural sample has too few over-window pairs to see the long-source result | Med | Med | Bucketed report, plus a Phase 0 sample of 60 |
| 400k-char calls time out | Med | Low | `claims-timeout-secs` from Phase 0 p99, and a per-trace error row (Phase 1 of the loop doc) |
| Overall trust passes while over-window stays weak | Low | Med | Phase 3 acceptance requires over-window n >= 10 at >= 0.80 on its own line |
| Over-window pairs whose evidence sits in the head inflate Phase 0 | Med | High | Bucket rule: the original number must first occur past char 24,000 |
| Phase 0 pollutes live trust rows | Low | High | `XDG_DATA_HOME` scratch copy, plus a criterion proving 0 new live rows |
| Rubric bump re-judges the whole corpus | High | Low | Expected. The backlog drains at `eval.limit` per day |

## Open Questions

(none)

## References

- `docs/handoff/claim-judge-full-source.md`
- `docs/design/2026-10-14-automated-note-quality-loop.md` and `-implementation-notes.md`
- `borg/patterns/judge-claims.md`, `borg/src/eval/{judge,trust,inject,scores,cache}.rs`, `borg/src/eval.rs`
