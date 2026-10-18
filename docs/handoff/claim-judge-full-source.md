# Handoff: claim-judge trust (2026-10-14 to 2026-10-18)

Branch: main

## Next action

Phase 3 of `docs/design/2026-10-17-claim-judge-full-source.md`: ship and re-measure. It waits on Scott's release go (the checkpoint was put to him 2026-10-18 evening). When he gives it:

1. `bump release` on main (ungated repo, `bump --gates` says none). Patch unless he says otherwise.
2. `otto deploy` on ripr. Not a bare `cargo install`: the deploy syncs `~/.config/sb/patterns/`, and the deployed `judge-claims.md` still carries the deleted cut-off rule until it does. A `v3` row judged by the old pattern text would be a lie.
3. On ripr, `~/.cargo/bin/sb borg eval --inject --sample 30`, paste the report into the implementation notes, and check the three Phase 3 bars: a `v3` `judge-claims` number-swap row for `claude-sonnet-5` with `sample_size >= 30` and `paired_catch_rate >= 0.80`; the over-window line n >= 10 and >= 0.80; `--remediate` prints no judge-claims trust failure. If any fails, record it verbatim and stop; do not re-run hoping.
4. Flip the doc Status to Implemented (all phases) and commit the notes.

Do not start another design doc, Phase 0, or panel round. The panel has run 3 design rounds and 1 implementation audit on this doc.

## What the problem was, in one paragraph

The quality loop lets a judge act only when trusted: paired catch >= 0.80 over >= 30 planted pairs (`docs/design/2026-10-14-automated-note-quality-loop.md`, `borg/src/eval/trust.rs` `judge_check_trust`). Claude's `judge-claims` read 0.69. Two causes, both now fixed on local main: the rubric told the judge to fail any claim past the 24k source cut (0.90 in-window, 0.00 past it), and the paired metric was note-level, so an original flagged on any other claim counted as a miss even when the judge caught the planted swap (25/27 over-window). Scott picked option B on 2026-10-18: score the planted claim, build the full-source judge, no more spikes.

## Commits on local main since fb4bdf5 (origin/main), none pushed

- `955769f` docs(handoff): the previous version of this file.
- `19d2edd` feat(eval): paired catch scored on the planted claim. `plant` returns `Planted { distilled, claim }`; `flags(check, metrics, claim)`. Closes the distiller doc as not pursued.
- `4dca6a6` feat(eval): Phase 1, full-source claims judging, `ClaimsOutcome`, `claims-source-max-chars` (400k) and `claims-timeout-secs` (90), `is_covered` cap predicate, rubric `v3`, cut-off rule deleted.
- `1eb86e3` feat(eval): Phase 2, per-class `sample_fixtures` growth, `fits | over-window | over-cap` report buckets.
- `482b616` docs(design): status and the `is_covered` NULL correction.
- `6f24bdc` fix(eval): split applies the claims cap (GEPA adapter regression), over-cap bucketed before fits.

`otto ci` green on every one. Deployed on ripr: still `sb v0.18.0`.

## What was verified by hand this session

The 17 claims Claude flagged on freshly distilled long notes (distiller Phase 0, `066ad856` scratchpad `results/chunked.jsonl`), checked against staged sources under `~/.local/share/sb/borg/stages/` (sessions in `body.txt`, videos in `distilled.yml` transcript, the article in `fetched.html`):

- 8 are in the source (judge false positives), 6 of them on session sources: `hv-29b2f05e` "3 to 29", `hv-16d375ce` rg -a and okta, `hv-2fe2ec93` case.yaml, is_gated, prompt-injection, `cl-56dd5199` Rob Pike, `cl-1ed09284` "every 4 hours".
- 7 are distiller errors: `hv-29b2f05e` "three reviews" x2 (windowing drops the 4th), `cl-2de2b08b` center pull, `cl-38cd120a` host files, `cl-33faf56c` dichotomy x2 and test suite.
- 2 arguable: `cl-38cd120a` Screen Studio, the 3-to-29 "them".

So the judge is noisy on long sessions. The claim-level metric does not hide that: the flag rate on originals stays printed, and once trusted `--remediate` will re-distill some clean notes on it. Not fixed, recorded in the 10-14 doc's Resolved Decisions.

## Open items

- Claude's claims judge returns `empty response` on some long sources (`hv-5194d942` 4/5 runs, `cl-38cd120a`, 8 calls in the claim-judge Phase 0). `borg/patterns/judge-claims.md` has no data frame around session sources. Phase 3's error ceiling (3/82 passed last time) will show whether it matters.
- `cl-31951716` was published from raw PDF bytes (extraction bug, unrelated).
- Session windowing drops the middle (`distillers/src/session.rs:168-175`); the only core error that reproduces on today's code. The distiller doc is closed; reopen only if `--inject` says the distiller, not the judge, is the floor.
- How many staged repo traces exceed 400k is unmeasured; the `--inject` over-cap bucket answers it.
- Who pushed fb4bdf5 to origin/main at 13:15 PDT is unresolved. `git reflog show origin/main` says `update by push` from this clone, so a push ran from this checkout; the session that ran it was not found before Scott redirected the work.

## Mistakes in this thread, so they are not repeated

- Two design docs and two paid spikes were built on a metric nobody questioned. Read the metric before fixing what it measures.
- A go rule built on a test set nobody re-checked against current code. Re-distill first.
- A subagent's root cause (`kind: null`) was relayed as fact. Replay the raw output first.

## Read first, in order

1. `docs/design/2026-10-17-claim-judge-full-source.md`: Resolved Decisions (2026-10-18 entries), Phase 3.
2. `docs/design/2026-10-17-claim-judge-full-source-implementation-notes.md`: Phases 1-2 and the audit section.
3. `docs/design/2026-10-14-automated-note-quality-loop.md`: Resolved Decisions, 2026-10-18 (the metric).
4. Panel syntheses: `~/.cache/review-panel/runs/8CyVkRCN/` (design), `~/.cache/review-panel/runs/fq6ZL3LN/` (audit).

## Sessions

- `a0e50790` (2026-10-18 evening): this session. The metric change, Phases 1-2, the audit.
- `066ad856`, `16686a04`, `fd0fa46d`, `3d292b36`: the earlier thread (see git history of this file).
