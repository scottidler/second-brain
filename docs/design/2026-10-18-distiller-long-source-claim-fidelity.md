# Design Document: Distiller Long-Source Claim Fidelity

**Author:** Scott Idler
**Date:** 2026-10-18
**Status:** Draft. Phase 0 went no-go (2026-10-18), so per the go rule this doc returns to Alternative 1: keep chunking, repair the reduce step.
**Review Passes Completed:** 5/5 on the reworked design (draft, correctness, clarity, edge cases, excellence); panel rounds 1-3 folded (round 3 reviewed the rework; rounds 1-2 reviewed the earlier chunk-repair design it replaces)

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
  - Per-call distill timeout: production constructors pass borg `fabric.timeout-secs` (600 default, `borg/src/config.rs:1085`) or cortex's (120, `cortex/src/config.rs:419`); the 60 in each distiller struct is only a struct default.
  - `max_claims` (`distillers/src/validate.rs:36-52`) allows 10 claims, plus 2 per extra chunk up to 24. All six single-call patterns also say "`claims`: maximum 10" (`distill-article.md:72`, `distill-video.md:85`, `distill-thread.md:42`, `distill-session.md:109`, `distill-voicenote.md:38`, `distill-repo.md:77`), so a single call on a long source gets 10.
  - Models: production sessions use `harvest.model`, falling back to `llm.model` (`borg/src/pipeline/session.rs:464-469`), which is `claude-sonnet-5` on ripr. Other kinds use `fabric.model`, empty on ripr, so fabric's `DEFAULT_MODEL`.
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

- One path per kind. The reduce input already carries the capture note (since 0117c8a, `parse.rs:720-746`); the chunk calls do not. With one call, the model reads the note and the whole source together.
- The enumeration, anchor and command-honesty rules already live in the single-call patterns, so they keep working.

### Data Model

- New config key `distill.single-call-max-chars` in `borg.yml`, and the same key in `cortex.yml`, because `sb cortex summarize --backfill` builds its own `Dispatcher` (`cortex/src/summarize.rs:70`). It counts SOURCE chars, before the capture note and the data frame are added. Its default comes from Phase 0. It replaces:
  - `SINGLE_CALL_TOKEN_THRESHOLD` routing in every distiller;
  - session `token_cap` and `harvest.token-cap`, deleted with their template line (`config/templates/borg.yml.example:277`);
  - `fabric.max-content-chars` as a cut on distill input. Distill calls pass `max_chars = 0` (no transport cut; `truncate_input` treats 0 as unlimited), because the source is already windowed to the ceiling. A transport cut would remove the tail of the request, which for sessions is the task restatement after `END DATA` (`parse.rs:959-967`). Other fabric callers keep `max-content-chars` (`run_command` takes `max_chars` per call, `vault/src/fabric.rs:162`).
- Image keeps today's `max_chars` and timeout. `Dispatcher::new` derives image's values from the article config (`distillers/src/dispatcher.rs:98-102`), so Phase 1 gives image its own explicit values instead.
- Above the ceiling, the existing `window_head_tail` moves to `distillers/src/parse.rs` and serves every kind. It pushes `window:<source_chars>><kept_chars>` into `meta.validation.bounds-truncations`.
- `ValidationMeta::is_degraded()` (`vault/src/distilled.rs:444-446`) returns true on a `window:` entry, so the receipt is `degraded=true` and `sb doctor` warns.
- Claim budget: `max_claims` (`distillers/src/validate.rs:48`) takes source tokens. `equivalent_chunks = ceil(source_tokens / 8000)`, budget `= min(10 + 2 x (equivalent_chunks - 1), 24)`, with 10 at or under 12,000 tokens. That is the chunked path's own rounding: at 25,000 tokens both give 16.
- The budget reaches the model. The six single-call patterns drop the fixed "maximum 10" and read a `Claim budget: N` line the distiller writes into the input, next to the capture note. Raising the Rust cap alone only moves the truncation point.
- Timeouts: `distill.timeout-base-secs` and `distill.timeout-secs-per-100k-chars` (seconds, values from Phase 0's measured latency). Per call, `timeout = min(base + per_100k x ceil(source_chars / 100,000), pipeline hard timeout - elapsed - 60)`. `vault::fabric::run_command` enforces it as its wall-clock timeout and kills the subprocess, so it ends before the 1800s pipeline timeout, which bounds the whole ingest.

### API Design

- No CLI change.
- Deleted: the distillers' `chunk_transcript` (`distillers/src/video.rs`; `vault::embedding`'s function of the same name stays), `build_reduce_input`, `build_thread_reduce_input`, `select_reduce_claims`, every `distill_long`, `CHUNK_TOKEN_TARGET`, `SINGLE_CALL_TOKEN_THRESHOLD`, and the patterns `distill-{article,video,thread,session,voicenote}-{chunk,reduce}.md`.
- `borg/src/eval/candidate.rs:77` `reaches_single_call` becomes "at or under the ceiling", so `--candidate-pattern` can score long traces.
- `regenerate_dispatcher` (`borg/src/eval/regenerate.rs:199-209`) distills sessions with the production session model (`harvest.model`, then `llm.model`), not `fabric.model`, so eval and production agree.
- Deleted with the chunk path: `distill.chunk-retries` (`borg/src/config.rs:311`, consumed only at `borg/src/pipeline/session.rs:479`, template line `:223`). `DistillConfig` is `deny_unknown_fields` (`config.rs:289`), so a host yml carrying the key would fail to load. ripr's `borg.yml` has no such key (grep, 2026-10-18).

### Implementation Plan

#### Phase 0: Prove one call beats chunking (zero committed code)
**Model:** opus
- Operator-run on ripr with a throwaway build, the same isolation as the claim-judge Phase 0 (scratch `XDG_DATA_HOME`, scratch patterns dir, live DB row counts checked before and after).
- Test set: the 15 flagged traces, `cl-56dd5199 cl-9e724b4b cl-ad229f9f cl-b14d7e33 cl-2db85dab hv-12ae5acc hv-16d375ce hv-29b2f05e hv-2fe2ec93 hv-5194d942 cl-0222aa9e cl-1ed09284 cl-2de2b08b cl-33faf56c cl-38cd120a`. `hv-12ae5acc` and `hv-16d375ce` share one body (same md5), so 14 distinct sources.
- Size probes: `hv-a158c300` (2.49M bytes) and `cl-31951716` (1.42M bytes, the largest video). `hv-a158c300` runs on both served models: the session model, and fabric's `DEFAULT_MODEL` through the article pattern, so each model is proven at the largest size.
- Copy all 17 trace dirs into the Phase 0 scratch tree first, so a `sb borg retention sweep` before Phase 4 cannot remove them.
- The throwaway build has two arms over the same traces:
  - **chunked:** current code, unchanged.
  - **one call:** threshold, `token_cap` and transport `max_chars` lifted, timeout 1,500s, and the claim budget scaled and written into the input with the patterns' fixed "maximum 10" replaced (Data Model), so both arms get the same claim allowance.
- It re-distills named staged traces through the full `Dispatcher` (`borg/src/eval/regenerate.rs:199` is the precedent) and judges claims on the full source (the claim-judge Phase 0 patch). Sessions go through the production session model in both arms, not `fabric.model`.
- Distill each test source 5 times per arm, and each size probe once in the one-call arm. Judge every output with both judges. Retry an errored judge call once.
- Record per call: the served model, the effective timeout, input tokens, wall time, and whether the call or the parse failed (`fallback_distilled`, zero claims).
- **Success criteria:**
  - Each size probe completes in one call on each model it runs on, with a non-fallback output of at least 10 claims, and claims anchored in all three thirds where the kind carries anchors. A failed probe records why: the window (an API context error) or the timeout. The ceiling default is the largest size that passed on every served model.
  - A table lands in this doc: per source, arm and run, both-judge flagged claims, claims per note, the share of anchored claims in each third of the source, fallbacks, and wall time.
  - Each of the 6 core errors is marked reproduced in the chunked arm (3+ of 5 runs) or baseline-unreproduced.
  - **Go rule** (one call ships only if all hold):
    - zero fallback or parse failures in the one-call arm;
    - the one-call mean of both-judge flagged claims per run is at or below the chunked mean;
    - claims per note are within `eval.candidate.max-coverage-cut` of the chunked arm;
    - on sources whose claims carry anchors (videos), each third's share of claims is at least 0.8 of the chunked arm's share for that third. This catches head bias and lost-in-the-middle;
    - every test-source call finishes inside its timeout and inside 1800s.
  - Live `eval_scores` and `judge_trust` row counts are unchanged after the run.
- If the go rule fails, this doc returns to Draft with Alternative 1.

##### Phase 0 results (2026-10-18, ripr)

- **Run:** a throwaway build of a686fec (a distillers routing patch plus a harness example, never committed) re-distilled the 14 test sources through the full `Dispatcher`, 5 runs per arm, 4 in flight. Every output's claims were judged against the full source by claude-sonnet-5 and gemini-2.5-pro (`max_chars` 0, 600s, one retry, the claim-judge Phase 0 pattern with the cut-off rule deleted). `hv-12ae5acc` was not run on its own: its body is byte-identical to `hv-16d375ce` (same md5), so the `hv-16d375ce` rows stand for both.
- **Arms:**
  - chunked: ripr's production values (`fabric.max-content-chars` 100,000, timeout 600s, session `token-cap` 12,000, `--maxTokens` 16384) and the deployed patterns, which are byte-identical to a686fec.
  - one call: routing threshold and `token_cap` lifted, `max_chars` 0, timeout 1,500s, the Data Model budget written as a `Claim budget: N` line at the start of the input, and the six single-call patterns reading it.
- **Served model:** every distill call in both arms and every probe went to `claude-sonnet-5`. fabric's `DEFAULT_MODEL` on ripr is `claude-sonnet-5` (`~/.config/fabric/.env`), the same as `llm.model`, so ripr has one served distill model. This answers the Dependencies line that could not read it.
- **Isolation held:** the live `eval-cache.db` reads `eval_scores` 4934 and `judge_trust` 11 before (17:16:23Z) and after the run, with 0 rows of either newer than the 17:16:27Z start. The harness never opens the eval DB. `sb-eval.timer` was stopped for the run and restarted after.
- **Calls:** chunked made 180 distill calls with 0 errors. One call made 70, with 1 error (`empty response`). The claims judge: claude-sonnet-5 returned `empty response` on 5 runs even after the retry (`hv-5194d942` 4 runs, `cl-38cd120a` 1), and gemini-2.5-pro never failed.

Per source, arm and run (r1/r2/r3/r4/r5). Flagged = claims that both judges call not supported. `m` = a verdict still missing after the retry. Anchored claims are binned by the anchor's share of the video's duration, summed over the 5 runs.

| source | kind | chars | arm | both-judge flagged | claims | anchored claims early/middle/late | fallbacks | wall s |
|---|---|---|---|---|---|---|---|---|
| `cl-56dd5199` | article | 83,554 | chunked | 0/0/0/0/0 | 11/11/14/12/12 | n/a | 0 | 59/72/70/71/79 |
| `cl-56dd5199` | article | 83,554 | one call | 0/0/0/0/0 | 14/14/14/14/14 | n/a | 0 | 79/70/72/89/118 |
| `cl-9e724b4b` | article | 43,213 | chunked | 0/0/0/0/0 | 10/10/10/10/10 | n/a | 0 | 45/37/40/42/39 |
| `cl-9e724b4b` | article | 43,213 | one call | 0/0/0/0/0 | 10/10/10/10/10 | n/a | 0 | 53/57/52/46/23 |
| `cl-ad229f9f` | article | 44,090 | chunked | 0/0/0/0/0 | 10/10/10/10/10 | n/a | 0 | 26/40/39/39/40 |
| `cl-ad229f9f` | article | 44,090 | one call | 0/0/0/0/0 | 0/10/10/10/10 | n/a | r1 yaml-parse-error | 39/40/39/41/38 |
| `cl-b14d7e33` | article | 215,357 | chunked | 0/0/0/0/0 | 10/10/11/11/8 | n/a | r1-r5 partial-chunk-failure | 44/62/54/72/45 |
| `cl-b14d7e33` | article | 215,357 | one call | 0/0/0/0/1 | 0/22/22/22/22 | n/a | r1 yaml-parse-error | 103/99/89/77/80 |
| `cl-2db85dab` | repo | 85,381 | chunked | 0/0/0/0/0 | 0/10/10/10/10 | n/a | r1 yaml-parse-error | 70/74/47/87/62 |
| `cl-2db85dab` | repo | 85,381 | one call | 0/0/0/0/0 | 14/14/14/14/14 | n/a | 0 | 72/77/75/81/76 |
| `hv-16d375ce` | session | 30,998 | chunked | 0/0/0/0/0 | 10/10/10/10/10 | n/a | 0 | 17/17/20/18/16 |
| `hv-16d375ce` | session | 30,998 | one call | 0/0/0/0/1 | 10/10/10/10/10 | n/a | 0 | 30/24/24/23/24 |
| `hv-29b2f05e` | session | 49,759 | chunked | 0/1/0/0/0 | 8/8/9/9/8 | n/a | 0 | 16/16/18/14/16 |
| `hv-29b2f05e` | session | 49,759 | one call | 1/0/0/0/0 | 10/12/12/12/12 | n/a | 0 | 18/18/21/20/18 |
| `hv-2fe2ec93` | session | 147,608 | chunked | 0/0/0/0/0 | 9/10/9/9/10 | n/a | 0 | 14/14/16/16/15 |
| `hv-2fe2ec93` | session | 147,608 | one call | 1/0/0/0/0 | 18/18/18/18/18 | n/a | 0 | 29/27/28/27/30 |
| `hv-5194d942` | session | 183,051 | chunked | m/m/m/0/0 | 10/9/10/9/9 | n/a | 0 | 14/14/15/14/13 |
| `hv-5194d942` | session | 183,051 | one call | 0/0/0/m/0 | 13/14/13/13/12 | n/a | 0 | 24/22/22/20/19 |
| `cl-0222aa9e` | video | 66,421 | chunked | 0/0/0/0/0 | 10/10/11/12/9 | 0/24/28 (0.00/0.46/0.54) | 0 | 31/32/34/26/23 |
| `cl-0222aa9e` | video | 66,421 | one call | 0/0/0/0/0 | 14/14/0/0/14 | 0/32/10 (0.00/0.76/0.24) | r3 yaml-parse-error, r4 fabric-error | 130/75/57/164/45 |
| `cl-1ed09284` | video | 34,656 | chunked | 0/0/0/0/0 | 10/10/10/10/10 | 27/0/23 (0.54/0.00/0.46) | 0 | 43/37/60/55/34 |
| `cl-1ed09284` | video | 34,656 | one call | 1/1/0/0/0 | 10/10/10/10/10 | 26/3/21 (0.52/0.06/0.42) | 0 | 57/63/36/36/43 |
| `cl-2de2b08b` | video | 53,172 | chunked | 0/0/0/1/0 | 10/9/10/10/8 | 23/8/16 (0.49/0.17/0.34) | 0 | 31/31/35/30/40 |
| `cl-2de2b08b` | video | 53,172 | one call | 0/0/0/0/0 | 12/12/12/12/12 | 41/10/9 (0.68/0.17/0.15) | 0 | 44/60/48/36/34 |
| `cl-33faf56c` | video | 78,145 | chunked | 1/0/1/0/0 | 11/13/14/10/10 | 12/28/18 (0.21/0.48/0.31) | 0 | 46/49/46/35/36 |
| `cl-33faf56c` | video | 78,145 | one call | 1/0/0/0/0 | 14/14/14/14/14 | 38/28/4 (0.54/0.40/0.06) | 0 | 51/93/46/43/34 |
| `cl-38cd120a` | video | 116,973 | chunked | 0/1/1/0/0 | 13/9/12/9/16 | 26/20/13 (0.44/0.34/0.22) | r5 reduce-selection-failed | 96/53/56/54/45 |
| `cl-38cd120a` | video | 116,973 | one call | m/0/0/0/2 | 16/16/16/0/16 | 38/23/3 (0.59/0.36/0.05) | r4 yaml-parse-error | 79/82/59/65/153 |

Arm totals:

| arm | outputs | fallbacks | both-judge flagged per run | claims per note | video anchors early/middle/late | distill call p50 / p90 / max s | output wall max s |
|---|---|---|---|---|---|---|---|
| chunked | 70 | 7 | 0.090 (6 over 67 judged) | 10.03 | 0.33 / 0.30 / 0.37 (88/80/98) | 16.6 / 42.0 / 87.2 | 96 |
| one call | 70 | 5 | 0.132 (9 over 68 judged) | 12.39 | 0.50 / 0.34 / 0.16 (143/96/47) | 45.1 / 89.3 / 153.5 | 164 |

- **One-call fallbacks, each traced to its raw output.** All five are single-call output errors. None is a window error or a timeout:
  - `cl-ad229f9f` r1: the claims list holds a stray `- set: null` item with no `text`. serde fails with `claims[3]: missing field text` (line 45), so the whole note falls back. (Corrected 2026-10-18: this line first blamed `kind: null`, but an explicit null kind already parses on a686fec; replaying the raw output through `parse_pattern_yaml` gave the error above.) Fixed in `fb4bdf5`: `PatternClaim.text` is serde-defaulted, and every caller drops empty claims.
  - `cl-b14d7e33` r1 and `cl-0222aa9e` r3: the `summary` string has no closing `"`, so the parse fails at `tldr:`.
  - `cl-38cd120a` r4: the whole YAML came back on one line.
  - `cl-0222aa9e` r4: fabric `empty response` after 164s.
- **Chunked fallbacks:**
  - `cl-b14d7e33`: `partial-chunk-failure` in 5 of 5 runs. All 8 calls returned each time, so a chunk's YAML fails to parse.
  - `cl-2db85dab` r1: a single call (repo), with an unescaped `"` inside `summary`.
  - `cl-38cd120a` r5: `reduce-selection-failed`.

Size probes, one call, once each:

| probe | pattern | source chars | input tokens | wall s | claims | anchors early/middle/late | result |
|---|---|---|---|---|---|---|---|
| `hv-a158c300` | session (session model) | 2,474,263 | 618,716 | 104 | 21 (budget 24) | n/a | pass. gemini-2.5-pro flags 0 of 21. claude-sonnet-5's judge reply continued the transcript instead of giving verdicts, twice |
| `hv-a158c300` | article (fabric's default model) | 2,474,263 | 618,570 | 48 | 0 | n/a | fail, `yaml-parse-error`. The call completed, so the window held. The model continued the session transcript instead of distilling it: the `hv-e5d240` failure `frame_input_as_data` exists for, and the article pattern has no data frame |
| `cl-31951716` | article | 628,353 | 157,092 | 16 | 0 | n/a | not a usable probe. Its staged source is undecoded binary (PDF bytes), and the model said so. It is an article, not the largest video |
| `ht-99d3c5` (added: the largest staged video) | video | 510,171 | 127,547 | 66 | 24 (budget 24) | 19/1/4 | pass, with anchors in all three thirds. Both judges flag 0 |

- **Ceiling default:** 2,474,263 chars, the largest size that passed on the one served model (`claude-sonnet-5`, `hv-a158c300` through the session pattern). No probe hit an API context error or a timeout.
- **Latency for the timeout keys:** one-call distill calls on the 14 sources (31k-215k chars) ran p50 45s, p90 89s, max 153.5s on success, and 164s for the empty response. The 618k-token probe took 104s. Wall time tracks output size, not input size: `hv-5194d942` (183k chars) ran p50 22s while `cl-56dd5199` (84k) ran p50 72s. No measured call used more than 164s of its 1,500s timeout, and the data shows no per-100k-chars slope.

Core errors in the chunked arm (reproduced = the error in 3+ of 5 runs):

| trace | chunked runs with the error | mark | evidence |
|---|---|---|---|
| `hv-29b2f05e` | 3/5 (r1, r2, r5) | reproduced | r1 "All three security reviews concluded no vulnerabilities...", r2 "Across all three reviewed diffs...", r5 "All three reviewed harness/schema-validation diffs...". One call: 0/5 state a review count |
| `cl-2de2b08b` | 1/5 (r4) | baseline-unreproduced | r4 "pulls the center to the backside linebacker", which both judges call contradicted. r3 says "ID linebacker", r2 "inside linebacker", r1 "the linebacker", r5 omits it |
| `cl-0222aa9e` | 0/5 | baseline-unreproduced | no run states the interviewer's hedged question as a position. r3 carries the founder's pushback, attributed, which both judges support |
| `cl-2db85dab` | 0/5 | baseline-unreproduced | r2 and r4 name the 8 languages. r1 fell back, and r3 and r5 omit it |
| `cl-9e724b4b` | 1/5 (r4) | baseline-unreproduced | r4 "a multi-region cluster combining a home Proxmox/Raspberry Pi infrastructure, Vultr cloud workers, and an OVH worker node" (gemini-2.5-pro unsupported). One call r1 makes the same merge |
| `hv-16d375ce` | 0/5 | baseline-unreproduced | no chunked run has the "Reusable pattern" lesson, and both judges flag no claim in any chunked run |

- **Go rule, as written:**
  - Zero fallback or parse failures in the one-call arm: 5 of 70. FAIL.
  - One-call mean both-judge flagged per run at or below chunked: 0.132 against 0.090. FAIL.
  - Claims per note within `eval.candidate.max-coverage-cut` (0.10): one call 12.39 against chunked 10.03, no cut (+23%). Pass. The key bounds a cut (`borg/src/config/eval.rs:18`).
  - Each video third at least 0.8 of the chunked share: early 0.50 against a 0.26 floor, pass. Middle 0.34 against 0.24, pass. Late 0.16 against 0.29, FAIL. Every long video moves claims out of the last third: `cl-2de2b08b` 0.34 -> 0.15, `cl-33faf56c` 0.31 -> 0.06, `cl-38cd120a` 0.22 -> 0.05.
  - Every test-source call inside its timeout and inside 1800s: max 164s. Pass.
  - **No-go: three of five clauses fail. This doc returns to Draft with Alternative 1.**
- **Beyond the rule:**
  - One call fixed the one core error that reproduced: `hv-29b2f05e` went from 3/5 to 0/5, so windowing was its cause.
  - 5 of 6 core errors do not reproduce on the current code in 5 runs, and both arms flag about 1 claim per 10 runs. Phase 4's "at least 3 of the 6 must be countable" bar would be inconclusive under either design.
  - Head bias is the loss that stands on its own. The fallbacks are output-format errors that both paths share.

#### Phase 1: One ceiling, one call
**Model:** opus
- Add `distill.single-call-max-chars` with the Phase 0 default, threaded through `Dispatcher` (`distillers/src/dispatcher.rs`) and `regenerate_dispatcher`.
- Every distiller takes the single-call path at or under the ceiling. Above it, head+tail window to the ceiling, record `window:`, mark degraded.
- Distill fabric calls pass `max_chars = 0`. Image gets explicit values equal to today's. Scale the claim budget and timeout as in Data Model, and write the `Claim budget: N` line; the six single-call patterns read it in place of "maximum 10".
- `regenerate_dispatcher` uses the production session model for sessions.
- Delete `harvest.token-cap` and session `token_cap`, and the template line.
- **Success criteria:**
  - `cargo test -p distillers long_source_is_one_call` passes: a 200,000-char article, video, thread, session and voicenote each make exactly one distill call through `FakeFabric`. The request holds the whole source, the capture note, the `Claim budget: 16`-style line, and for sessions the closing task after `END DATA`.
  - `cargo test -p borg single_call_ceiling_reaches_every_entry_point` passes: a non-default ceiling in `borg.yml` reaches the final `FabricRequest` through ordinary ingest, harvest/session, eval regenerate and candidate; the same in `cortex.yml` through `summarize --backfill`. Image's `max_chars` and timeout are unchanged.
  - `cargo test -p distillers over_ceiling_records_window` passes with an explicit 1,000-char ceiling: the entry is present and `is_degraded()` is true. It fails with the push removed.
  - `cargo test -p borg regenerate_dispatcher_uses_single_call_ceiling` passes: `regenerate_dispatcher` built from a borg config with a 300,000-char ceiling distills a 200,000-char article in one call. It fails if the eval dispatcher falls back to the default.
  - `rg -n "token-cap" config/templates/borg.yml.example` prints nothing.

#### Phase 2: Delete the chunk/reduce path
**Model:** sonnet
- Delete the symbols and ten patterns listed in API Design, and their tests.
- Remove the ten entries from `PATTERNS` in `sb/src/cli/bootstrap.rs:79-140`, which embeds them with `include_str!`.
- Delete `distill.chunk-retries`: the field, its wiring, its tests and its template line.
- Update `reaches_single_call`, `distillers/AGENTS.md`, and the CLAUDE.md patterns count.
- The ten deployed copies under `~/.config/sb/patterns/` stay on each host: bootstrap writes patterns but never prunes them (`sb/src/cli/bootstrap.rs:315`), and nothing reads them after this phase.
- **Success criteria:**
  - `rg -l "fn distill_long|select_reduce_claims|build_reduce_input|build_thread_reduce_input|SINGLE_CALL_TOKEN_THRESHOLD|CHUNK_TOKEN_TARGET|chunk_retries|chunk-retries|-chunk\.md|-reduce\.md" distillers borg sb cortex config | wc -l` prints `0` (19 on main). The search names distiller symbols only: `vault`'s embedding code has its own unrelated `chunk_transcript`, which stays.
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
- **Success criteria:**
  - `rg -l "CLAIM-HONESTY" borg/patterns | wc -l` prints `6`, and the six blocks are byte-identical.
    - Observed on main: `0`.
  - `rg -c "stated in the session" borg/patterns/distill-session.md` prints at least `1`, on the "Reusable patterns" bullet (`distill-session.md:52-53`, today "a technique, command, or structure that generalizes beyond this one task").
    - Observed on main: no match (exit 1).
  - Each candidate run, forced fresh, reports no coverage cut beyond `eval.candidate.max-coverage-cut`.

#### Phase 4: Re-measure (operator, ripr)
**Model:** opus
- The Phase 0 one-call arm, rebased on the shipped code, 5 runs per test source.
- Each core error that reproduced in Phase 0's chunked arm gets one outcome:
  - **reproduced:** the error appears in any run (this beats the other two);
  - **corrected:** a claim stating the source-backed fact is present in 3+ of 5 runs and both judges call it supported;
  - **omitted:** no claim on that fact in 3+ of 5 runs.
- The fact each core error must keep, and whether omission counts:

  | trace | the fact that should survive | omitted counts as fixed |
  |---|---|---|
  | `hv-29b2f05e` | four security reviews ran | no |
  | `cl-2de2b08b` | the center pulls to the ID linebacker | no |
  | `cl-2db85dab` | community translations in 8 languages (9 repos) | no |
  | `cl-9e724b4b` | the six-node Proxmox/Vultr/OVH cluster, separate from the 3-Pi production cluster | no |
  | `cl-0222aa9e` | the interviewer asked, hedged, whether generalization is harder for decisions | yes |
  | `hv-16d375ce` | none: the lesson was invented | yes |

- A baseline-unreproduced error is reported, never counted as fixed.
- Missing verdicts:
  - an error seen in any run is reproduced, even when other runs of that source miss verdicts;
  - otherwise, a source with more than one run missing a verdict after the retry is inconclusive and leaves both counts;
  - the both-judge mean is divided by the runs with complete verdicts, never by 5.
- **Success criteria:**
  - Every countable core error is corrected, or omitted where the table allows it. At least 3 of the 6 must be countable, or the result is inconclusive.
  - Zero fallback or parse failures.
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
- [ ] Phase 4's table meets its bar: every countable core error corrected (or omitted where its row allows), zero fallbacks, and the both-judge mean down by half.
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
- 2026-10-18 (panel round 3, first review of the one-call design): 7 must-fixes and 7 cheap wins folded in:
  - the `bootstrap.rs` pattern embeds and the widened Phase 2 search;
  - the claim budget written into the input, with the chunked rounding;
  - `max_chars = 0` so the session task trailer survives;
  - image's explicit values;
  - the session model in eval and Phase 0;
  - zero fallbacks, per-third coverage, and per-error survival facts in the go rule and Phase 4;
  - `distill.chunk-retries` deletion (ripr's yml checked);
  - the capture-note and timeout facts corrected;
  - a ceiling proven per served model;
  - an entry-point config test;
  - Phase 3 criteria;
  - the stale deployed patterns stated;
  - the timeout keys and budget.
- 2026-10-18 (panel round 3, deferred with reason): daemon-worker starvation from long calls is unquantified, and the HEAVY_PERMIT already serializes heavy ingests. Phase 0's wall times show whether it matters. The architect's generic rollback ask is covered by one Rollout line. The architect's "Phase 2 lacks `cargo test`" is wrong: `otto ci` runs the tests.
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
- Rollback: the previous tag + `otto deploy`. Its bootstrap rewrites the ten chunk/reduce patterns.

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
