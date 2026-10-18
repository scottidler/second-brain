# Handoff: claim-judge trust, and the distiller detour (2026-10-14 to 2026-10-18)

Branch: main

## Next action

Get Scott's go on the direction below, then do step 1. He was shown it on 2026-10-18 and has not answered yet. Do not start a new design doc, a new Phase 0, or a review round before he answers. Three days of loops on this thread came from starting the next experiment before the last one's result was understood.

Direction proposed to Scott (2026-10-18, this session):

1. Close `docs/design/2026-10-18-distiller-long-source-claim-fidelity.md` as not pursued. Keep its Phase 0 numbers as the record. Its Status today reads Draft -> Alternative 1. That branch was written before anyone saw that the test set no longer reproduces (below), so do not build Alternative 1 either.
2. Re-run the claim-judge Phase 0 (`docs/design/2026-10-17-claim-judge-full-source.md`, Phase 0) with the originals freshly distilled by today's code, so stale published notes do not pollute the paired metric.
3. If it passes its go rule, build that doc as written with `/how-to-execute-a-plan`.

## The goal, in one paragraph

The automated note-quality loop (`docs/design/2026-10-14-automated-note-quality-loop.md`, shipped v0.18.0) only lets a judge act (`--remediate`, `--apply-candidate`, the GEPA candidate search) once it is trusted: paired catch rate >= `eval.trust.min-paired-catch-rate` (0.80) over >= 30 planted pairs per class (doc lines 183, 459, 467; `borg/src/eval/trust.rs:181` `judge_check_trust`). Paired catch = the judge flags the planted mutant AND passes the original. Claude's `judge-claims` number-swap trust row reads 0.69 (`judge_trust`, measured 2026-10-17T15:06:59Z), so it is untrusted and the loop records but never acts. Every piece of work below is in service of getting that judge trusted.

## What happened, in order

### 2026-10-17: claim-judge full-source design (session `fd0fa46d`, then `16686a04`)

- Cause found: the judge-claims rubric told the judge to fail any claim sourced past the 24k input cut (`borg/patterns/judge-claims.md:21-22`). Claude scored 0.90 on sources that fit and 0.00 past the cut.
- Design: `docs/design/2026-10-17-claim-judge-full-source.md`. Judge on the full source up to `eval.claims-source-max-chars` (400k); a larger source is `not judgeable`. Phase 0 = a paid spike on ripr before any code.
- Panel: synthesis at `~/.cache/review-panel/runs/8CyVkRCN/synthesis.md`. Round counter for that doc reads `rounds=5` (`~/.cache/review-panel/rounds/c129ed7a...`), so any further round needs `PANEL_ROUNDS_ORDERED_BY_SCOTT=<n>` and Scott's OK.
- Tooling fixed along the way, all in `scottidler/claude`, pushed:
  - `a731349`, `5d6cb24`: the gemini architect seat's `pathglob.py` hook, which gets past gemini-cli's untrusted-context taint checks on path and flag-value arguments.
  - `31bed2c`: review-panel rounds counted at PostToolUse, so classifier-denied dispatches no longer pad the cap. The PostToolUse(Agent) hook is registered in `~/.claude/settings.json`, and `panel-round-guard.sh --self-test` reads `pass=125 fail=0` (re-checked 2026-10-18). The previous version of this handoff called this blocker open. It is closed.

### 2026-10-18 01:00Z: claim-judge Phase 0 ran, no-go (session `16686a04`)

- Results: the "Phase 0 results (2026-10-18, ripr)" section of the claim-judge doc. Commit `0f15d33`.
- Both judges failed the over-window bar: 0.59 (claude) and 0.57 (gemini), against 0.80.
- But at claim level the judges caught the planted swap on 25/27 and 26/27 over-window pairs. The pairs missed because 11-12 of 29 long-source ORIGINAL notes were already flagged by some other claim.
- Scott's decision (in that doc, `e7adaf8`, `32cd804`): fix the distiller first, then re-run Phase 0. Test the fix on the 15 already-flagged originals, with 6 both-judge-flagged claims as the core errors.
- Harness for this run: `/media/saidler/samsung-4tb-ssd/claude-tmp/claude-1000/-home-saidler-repos-scottidler-second-brain/16686a04-49c3-4953-91b6-2b04e8f0f3f7/scratchpad/` (`phase0.patch`, `phase0.sh`, `report.txt`, `flagged/`). Run tree on ripr: `~/phase0/`. Both are session scratch and may be reaped.

### 2026-10-18 morning: distiller fidelity design, reworked twice (session `16686a04`)

- `docs/design/2026-10-18-distiller-long-source-claim-fidelity.md`. Commits `4c8e79a` through `a686fec`.
- First design: keep chunking and repair the reduce step. Panel rounds 1-2: `~/.cache/review-panel/runs/v2XFzIL6/synthesis.md`.
- Scott, mid-design: "llms have a 1million token window. why the fuck would it take 20 or 100 llm calls?" The 12,000-token single-call line traces to an unmeasured number in `docs/design/2026-05-16-extractor-contract-and-l2-summaries.md:592-595`, implemented as `SINGLE_CALL_TOKEN_THRESHOLD` (`distillers/src/video.rs:36-38`).
- Rework (`f5c0c9f`): one call per source up to a measured ceiling, chunk/reduce deleted, plus a claim-honesty rule. Panel round 3: `~/.cache/review-panel/runs/MqV1BgYE/synthesis.md`. Round counter `rounds=3`.

### 2026-10-18 17:10-19:10Z: distiller Phase 0 ran, no-go (this session, `066ad856`)

- Results: the "Phase 0 results (2026-10-18, ripr)" section of the distiller doc, with per-source tables. Implementation notes: `docs/design/2026-10-18-distiller-long-source-claim-fidelity-implementation-notes.md`. Commit `9d281af`.
- Go rule, by clause:
  - one-call fallbacks 5/70: FAIL. Chunked had 7/70.
  - both-judge flagged claims per run: one call 0.132 against chunked 0.090: FAIL.
  - claims per note: pass.
  - video late third: 0.16 of claims against 0.37 chunked, under the 0.29 floor: FAIL. This is a real start bias on long videos.
  - timeouts: pass. Max 164s.
- Size is not a problem: one call handled a 618,716-token session (`hv-a158c300`) in 104s with 21 claims. Ceiling measured: 2,474,263 chars.
- **The finding that matters, and the doc's go rule never asked it:** re-distilled with TODAY's code, the 15 already-flagged notes mostly come back clean.
  - Per judge, notes with any flagged claim: chunked 14/60 (claude) and 15/63 (gemini). The published versions were 100% flagged by construction of the set.
  - Core errors that reproduce on today's chunked code: 1 of 6 (`hv-29b2f05e`, session windowing, 3/5 runs; one call fixes it 0/5).
  - So the long-source claim errors live mostly in stale published notes from older code and patterns, not in today's distiller. Recompute: `results/*.jsonl` in this session's scratchpad (below), counting each judge's `verdicts` per output.
- Cost of the run: about $28.17 over 543 calls, token counts estimated at chars/4 (fabric reports no usage). The table is in the implementation notes.
- Run artifacts, session scratch, may be reaped:
  - local: `/media/saidler/samsung-4tb-ssd/claude-tmp/claude-1000/-home-saidler-repos-scottidler-second-brain/066ad856-83c5-4155-8ba0-112346f3a8ed/scratchpad/` (`phase0-distill.patch`, `results/`, `raw/`, `analyze.py`, `cost.py`)
  - on ripr: `~/phase0-distill/`

### 2026-10-18 afternoon: the parse fix (this session)

- The Phase 0 worker blamed one fallback on `kind: null`. Wrong: an explicit null kind already parses (proved by a test that passed with and without the "fix"). Replaying the raw output through `parse_pattern_yaml` showed the true cause: a stray `- set: null` claims item, `claims[3]: missing field text`.
- `fb4bdf5`: `PatternClaim.text` is `#[serde(default)]` with a WARN in `into_claim` (`distillers/src/parse.rs:448-485`). Test `claim_item_without_text_is_dropped_not_fatal` (`distillers/src/article/tests.rs`) fails without the default. The real ripr raw output now parses to 10 claims. `otto ci` green.
- The distiller doc's fallback bullet is corrected in the same commit as this handoff.
- The other one-call parse failures are YAML syntax errors: two unclosed `summary` strings and one output written on one line. Nothing fixes those yet.

## Where things stand

- `second-brain` main == origin/main at `fb4bdf5` (pushed 2026-10-18 13:15 PDT, not by this session). This handoff commit is local until pushed.
- Deployed on ripr: `sb v0.18.0`. Nothing after `c2699e3` is released. `fb4bdf5` is the only code change since, and needs `bump release` + `otto deploy` when Scott OKs it.
- Live eval DB untouched by both Phase 0 runs: `eval_scores` 4934, `judge_trust` 11 at 17:16Z on 2026-10-18. `sb-eval.timer` is active. Probe: `sqlite3 -readonly ~/.local/share/sb/borg/eval-cache.db "select count(*) from eval_scores; select count(*) from judge_trust"`, and `systemctl --user is-active sb-eval.timer`.
- Claude trust row probe: `sqlite3 -readonly ~/.local/share/sb/borg/eval-cache.db "select judge_model, check_name, planted_class, rubric_version, paired_catch_rate, sample_size from judge_trust where check_name='judge-claims'"`.

## Open items the next agent must not lose

- **The claim-judge Phase 0 re-run needs a fresh-originals mode.** Today `--inject` takes the published note as the original. The distiller Phase 0 harness (`phase0-distill.patch`) already re-distills named traces through the production `Dispatcher` (precedent `borg/src/eval/regenerate.rs:199`). Combining it with the claim-judge patch (`16686a04/scratchpad/phase0.patch`) is the shortest path. Scott has not approved this yet.
- **Claude's claims judge returns `empty response`** on some long sources (`hv-5194d942` 4/5 runs, `cl-38cd120a`; and 8 calls in the claim-judge Phase 0). On the 2.47M-char session it continued the transcript instead of returning verdicts. `borg/patterns/judge-claims.md` has no data frame around session sources. This will hit the claim-judge re-run.
- **`cl-31951716` was published from raw PDF bytes.** Its staged source is undecoded PDF. It is an extraction bug, unrelated to claims.
- **Start bias on long videos with one call** (late third 0.16 against 0.37). Only matters if one call is ever revisited.
- Gemini engine empty first window (`cl-0222aa9e` starts at `[00:30:00]`, `borg/src/youtube/gemini.rs`): parked in the distiller doc's Non-Goals.

## Mistakes in this thread, so they are not repeated

- A go rule built on a test set nobody re-checked against current code. 5 of 6 core errors were stale before Phase 0 started. Before any "fix X, measure on the flagged set" plan, re-distill the set with today's code first. It costs 1 arm.
- A subagent's root cause was relayed as fact (`kind: null`) and a fix was written for it before the raw output was replayed. Replay first.
- Each design was reworked mid-flight (chunk-repair -> one call), and each rework re-armed panel rounds. Two docs now sit at 3 and 5 rounds.

## Read first, in order

1. `docs/design/2026-10-17-claim-judge-full-source.md`, the Phase 0 results section and Resolved Decisions.
2. `docs/design/2026-10-18-distiller-long-source-claim-fidelity.md`, the Problem table, then the Phase 0 results section.
3. `docs/design/2026-10-18-distiller-long-source-claim-fidelity-implementation-notes.md`, including the cost table.
4. `docs/design/2026-10-14-automated-note-quality-loop.md`, the trust rule (lines 183, 459, 467).
5. Panel syntheses: `~/.cache/review-panel/runs/8CyVkRCN/`, `v2XFzIL6/`, `MqV1BgYE/`.

## Sessions, for deeper recall

- `fd0fa46d` (2026-10-17): v0.18.0 deploy, the first claim-judge handoff, the architect-seat gemini work.
- `16686a04` (2026-10-18 00:18-17:02Z): claim-judge design rounds, claim-judge Phase 0, both distiller designs and their panels.
- `066ad856` (2026-10-18 17:02Z on): distiller Phase 0, the parse fix, this handoff.
- `3d292b36` (2026-10-17): the note-quality loop build that created the trust gate.

## Suggested skills

- `session-recall` for any of the sessions above.
- `how-to-execute-a-plan`, only after Scott OKs the direction and the claim-judge doc is updated for fresh originals.
- `review-panel` only if Scott orders a round. Both docs are at or past the cap.
- `bump` (release-driver) for `fb4bdf5` when Scott OKs a release.
