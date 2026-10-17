# Implementation Notes: Jev Tag Classifier

## Phase 0b: Calibration and ship gate
### Design decisions
- Winner C at 0.90 (P 0.840, R 0.650, F1 0.733, instability 0.065), design doc Phase 0b results: the highest F1 of the four rows that clear P ≥ 0.80 and instability ≤ 0.195.
- `state` built in the probe the way the code builds it: frontmatter `title` (or `Untitled`, as `cortex/src/classify.rs:824`), body split per `vault::frontmatter::split_raw` plus the leading-newline trim, `## Summary` per `vault::search::parse_body_summary`, else the first 900 chars of the body (`distillers::tags::body_excerpt`). 39 summaries, 7 body heads.
- Scoring reimplements `p0/score.py` with the threshold as a parameter: ≥ threshold, highest first, cap 8; micro P/R/F1 per run, averaged over the two runs; note-level instability between run 1 and run 2. `score.py` hardcodes `TH = 0.9` and the 53-note gold list, so it could not be called as is.
- Data path for C, written into Data Model: a top-level `descriptions:` map in `canonical-tags.yml` (tag -> text) instead of reshaping `tags:` (group -> list), so the file shape and `CanonicalTagsFile.tags` stay unchanged. A non-canonical key is a load error; a tag without a description sends no `criteria`.
### Deviations
- Kept 1 renamed gold note (`ex-snowflake-engineers-...-so-they-built-tower-to-fix-it.md` -> `...-so-they.md`, same title and source URL) instead of dropping it as missing. The spec says drop notes that no longer exist; this one exists under a new path.
- Canonical `kubernetes` was renamed `k8s` (`939470e`) after gold and `descriptions.json` were written. Gold's `kubernetes` is scored as `k8s`, and arm C's `k8s` criterion uses `descriptions.json["kubernetes"]`. Without the mapping arm C cannot be built (117 canonical tags, 116 descriptions match).
- Filled the threshold value (0.9) into Phase 3 and operator step 1 as well; the spec named only Data Model, API Design and Phase 1.
### Tradeoffs
- 8 concurrent requests per run vs serial: Phase 0 showed 60 concurrent calls with no 429, and 8 keeps each run short without probing the rate limit.
- Transient retry in the probe (429/5xx/transport, up to 4 tries) vs fail on first error: the probe did not log retries, so whether one fired is unrecorded; every note in all six runs ended with a 200 carrying an answer per tag.
### Open questions
- C at 0.90 F1 (0.733) is below the classifier.dev baselines A0 (0.741) and A2 (0.788), which were measured on 53 notes and `wire.json`'s 900-char clip. The gate passes; whether that gap matters is Scott's call.
- 7 of 53 gold notes are gone (vault commit `6ab13d50`), so gold is now 46 notes. Refresh gold before the next calibration?
