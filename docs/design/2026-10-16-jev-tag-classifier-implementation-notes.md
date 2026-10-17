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

## Phase 1: Jev classifier, callers compiled green
### Design decisions
- The non-canonical `descriptions:` check runs on the serde path, not in `load` (`vault/src/canonical.rs:CanonicalTagsFile`, via `#[serde(try_from = "RawCanonicalTagsFile")]`): `sb cortex tag-promote` parses its candidate buffer with `serde_yaml::from_str`, never `load`, so a check living only in `load` would miss the one path that writes the file.
- `descriptions:` sits at the end of `config/canonical-tags.yml`, one double-quoted line per tag in group order: `cortex/src/proposals.rs:insert_tags_into_group` finds a group by an exact `  <group>:` line and ends a group at the first non-entry line, so a trailing block of `  <tag>: "..."` lines can never be mistaken for a group header.
- The key resolves once in `Jev::new` through `vault::config::resolve_secret`, trimmed, unwrap-or-empty (`distillers/src/tags.rs:Jev::new`): same as `GeminiClient::from_config`; an unresolved name errors at `classify`, before any request, naming the variable. `Jev` derives no `Debug`, so the key cannot reach a log line through `{:?}`.
- One deadline per `classify`: each request's ureq `timeout_global` is what is left of `Instant::now() + timeout-secs`: `distillers/src/tags.rs:Jev::send`.
- 429: the retry runs only if `delay + RETRY_REQUEST_ALLOWANCE` (1s) fits what is left of the deadline (`distillers/src/tags.rs:Jev::post`): the doc says "delay plus a request" without saying how long a request is, so 1s stands in for one (Phase 0 measured 0.08-0.92s).
- Error text: status, `detail.error_type`, `detail.message`, `x-typesafe-request-id`: `distillers/src/tags.rs:api_error`. A FastAPI 422 (`detail` is a list) reports `validation_error` plus each item's `loc: msg`, so `input` is dropped. A body that is not JSON contributes nothing, because a 5xx body could hold anything.
- `noul` missing or null counts as malformed (`Err`), the same as out of range: `distillers/src/tags.rs:Jev::scores`.
- The live fixture `distillers/src/tags/fixtures/jev-2026-10-16.json` was recorded on ripr: one synthetic Rust-CLI note, all 117 questions with criteria. 200 in 0.14s, served `jev-1.13.0`. Body checked for the key: 0 hits, saved verbatim. It carries `expected` (the ≥0.9 set) for the 200 stub test.
- The stub tests read the key from a temp secret file (`api-key: <path>`) rather than setting an env var, so no test mutates the process environment: `distillers/src/tags/tests/jev.rs:jev_for`.
- Break-the-code evidence (each mutation applied, the tests run, the file restored): a second `post` per classify → 14 jev tests fail, incl. the 1-request test; dropping `criteria` → 2 fail; appending the response body to the error → the 422 and 500 tests fail; removing the empty-key guard → both key tests fail; removing the empty-text guard → `empty_text_sends_no_request` fails.
### Deviations
- `finish` now takes one score list (`&[(String, f32)]`) instead of per-shard maps. The spec says keep `finish` unchanged, but its input was `merge_shard_scores` output, and that function is deleted. Same rules: author tags at tier 0, cap, subset guard, High/Low.
- `sb/src/cli/checks.rs` auth line: the compile fix reads `api-key` through `resolve_secret` ("key from X" / "X names no value"). The hint still names classifier.dev; Phase 2 owns that wording.
- `borg/src/pipeline/tags/tests.rs:classifier_failure_degrades_visibly` had to change to compile. It set `token_env`/`endpoint` and `ClassifierKind::ClassifierDev`, not only the `TagMethod` the spec lists. I used the change Phase 2 specifies (an api-key name that resolves empty, no network) rather than invent an interim one. Phase 2's remaining borg work (stub 200/500 pipeline tests) is untouched.
- `sb/src/cli/checks/tests.rs:cortex_drift_uses_the_vault_the_unit_was_installed_with` now repoints `XDG_CONFIG_HOME` at an empty temp dir, which meant making `drift::EnvGuard` `pub(super)`. Root cause: `cortex::daemon::desired_systemd_unit` loads `vault::paths::cortex_config()`, and the test never repointed XDG, so it parsed ripr's live `~/.config/sb/cortex.yml`. That file still says `classifier: classifier-dev` (operator step 1), which the new binary rejects. The test had depended on host state all along; this change only made that visible.
- 11 `CanonicalSet { .. }` struct literals across vault/sb/borg/cortex/distillers tests gained `description: HashMap::new()`. That is mechanical, forced by the new field.
- Deleted classifier.dev tests, replaced as follows:
  - `the_default_config_names_no_token_env` → inverted as `the_default_config_names_the_typesafe_key_and_jev_latest`
  - `an_unset_or_blank_token_env_resolves_keyless` → inverted as `an_unresolved_api_key_errors_without_a_request` + `a_blank_key_file_errors_without_a_request` (there is no keyless path now)
  - `only_jev_answers_are_accepted` → `a_non_jev_model_is_an_error`
  - `shards_are_sorted_and_merge_to_recorded_scores`, `the_text_chunk_ceiling_is_the_public_tier_limit`, `the_request_uses_the_multi_field` → deleted with the code they pinned
  - `outputs_are_canonical_and_capped` lost its classifier.dev-fixture half; the jev 200 test now asserts the cap and the vocabulary subset against the new fixture.
### Tradeoffs
- `try_from` raw-struct split vs a validate call in `load`: covers every parse path at the cost of duplicating the field list once.
- A hand-rolled `std::net` HTTP/1.1 stub vs pulling a server crate into distillers dev-deps: no new dependency; ureq sends `content-length`, so the stub only reads that framing.
- No `classifier-dev`-value rejection test: the Phase 1 grep forbids the string in `distillers/src`. The stale `token-env` / `endpoint` / `base-url` keys are tested instead, and serde's unknown-variant error covers the old variant (seen live in the sb failure above: `unknown variant ..., expected one of deterministic, fabric-closed, jev`).
### Open questions
- `the_shipped_file_describes_every_canonical_tag` pins a description for every canonical tag, as the spec asks. `sb cortex tag-promote --apply` writes no description, so the first promotion turns that test red until someone adds a `descriptions:` line. That forces a description for every new tag, which arguably is the point. Keep it, or relax it to "every description key is canonical" only?
- ripr's live `~/.config/sb/cortex.yml` (and `borg.yml`) still say `classifier-dev`. Any `sb` built from this commit fails to load them until operator step 1. Expected per the doc's rollout, but worth knowing before any interim `otto install` on ripr.

## Phase 2: Borg and cortex wiring, doctor, daemon-start check
### Design decisions
- One shared check, `TagsClassifierConfig::empty_jev_key_warning` (`distillers/src/tags.rs`), returns the WARN text when `classifier` or `fallback` is `jev` and `api-key` resolves empty. Borg (`Config::validate_daemon_start`, `borg/src/config/engine.rs`) and cortex (`daemon::validate_daemon_start`, called from `start_watching` in `cortex/src/daemon.rs`) both log it. The `tags.classifier` block is identical in both ymls, so the condition lives once beside the config type. The warning names the field, the variable and the keep secret `escote-typesafe-api-key`.
- The borg warn runs before the youtube early return in `validate_daemon_start`, which used to return for any non-gemini config.
- The borg 200/500 pipeline tests reuse `crate::youtube::engine::stub::serve` (route `/v1/systemone`): it reads the full content-length body before answering, which the older single-read `borg/src/stub.rs` does not. Key rides a temp secret file, so no env mutation. The stub is served by the `#[tokio::test]` current-thread runtime and `finalize_tags` runs sync ureq on `spawn_blocking`, per the doc's Testing Strategy warning.
- The tests prove jev ran from the data, not a flag: the 200 test passes no candidates, so `deterministic` would return nothing and `rust` can only come from jev's 0.99. The 500 test passes a model candidate `llm`, which only `deterministic` consumes.
- `sb doctor`: the hint now names `api.typesafe.ai`, `tags.classifier.api-key` and `escote-typesafe-api-key`. The probe already posted to `/v1/systemone` through `distillers::tags::build` (the endpoint is `Jev`'s only one), so no code change there; the doc comment says so.
- `classifier_failure_degrades_visibly` (Phase 1) confirmed to meet the bullet: an api-key name that resolves empty, no network.
- Comments updated in `borg/src/pipeline/tags.rs`, `borg/src/pipeline/handlers.rs`, `cortex/src/classify.rs`.
- Bite checks (mutate, run, restore): 200 test with the stub returning 500 fails (`left: []`); `Ok(out) => (out.tags, false)` on the fallback arm fails the 500 test and `classifier_failure_degrades_visibly`; the borg and the cortex WARN each replaced by a no-op fails their daemon-start test.
### Deviations
- The spec says the 200 case asserts `method == Jev`. `TagOutcome` carries no method, and adding a field only for a test is the wrong seam. The tag set (only jev's score can select `rust`) and the single stub hit stand in for it.
- The spec calls for a daemon-start WARN "in cortex" with the same shape as borg. Cortex had no `validate_daemon_start`, so a small one now exists in `daemon.rs` and is called from `start_watching`.
- No `sb doctor` test: `classifier_findings` loads the live borg config and makes a live request, as before; there is no seam to stub without new code the spec does not ask for.
### Tradeoffs
- A shared method on the config type vs two copies of the condition: one place for the message and the rule.
- WARN not error on an empty key, even when `classifier: jev` and the fallback is `deterministic`: matches the doc and the gemini fallback precedent; ingest keeps working, degraded.
### Open questions
- None.

## Phase 3: Templates and agent docs
### Design decisions
- Both templates keep `classifier: deterministic` live and the jev keys (`threshold`, `fallback`, `api-key`, `model`, `timeout-secs`) commented — config/templates/{borg,cortex}.yml.example — field names read from `TagsClassifierConfig`; shipped default is unchanged.
- The root CLAUDE.md Tags bullet and distillers/AGENTS.md name jev and the per-tag `descriptions:` map from Phase 1.
### Deviations
- The sweep also hit a doc comment in `cortex/src/classify.rs` and two doc comments in `borg/src/pipeline/tags/tests.rs`; reworded (comments only) since the goal is no mention outside docs/.
### Tradeoffs
- Commented jev keys vs live ones — a live `api-key` would imply it is in use under the `deterministic` default.
### Open questions
- None.
