## Phase 0: spikes
### Design decisions
- Ran 0a/0b in a throwaway worktree on disk (`~/.cache/sb-0a`), removed afterward; 0d as a std-only scratch program; nothing committed — Phase 0 writes no repo code.
- Probe written as `cargo tree ... -i vault@<version>` plus a `dev`-edge positive control — Addendum D 0a — the bare `vault` spec was rewritten to a directory path by the session environment, which would have made the "prints nothing" check pass vacuously.
- `ntfy.read-timeout` default = 135s (3 x measured 45 s) — Addendum D 0c.
- `perf_search_vector_21k` ceiling for Phase 1 = 450 ms (10x release p50 44.37 ms) — Addendum D 0b.

### Deviations
- 0c measured against ntfy.sh with a throwaway topic, not the "live ntfy subscription": the live `borg.yml` has no `ntfy:` section, so there is no subscription to observe. Same URL shape and client path as `borg/src/ntfy.rs`; the keepalive is a server-side setting.
- 0a/0b worktree placed under `~/.cache` instead of `$TMPDIR`: the sandbox tmpfs was 99% full and `cargo` (an unsandboxed command) hit `No space left on device`.

### Tradeoffs
- 5-minute single-topic observation vs. a longer window — the interval was constant to 3 ms across 6 keepalives; a longer run would add nothing to a fixed server timer.
- 20 timed runs on an in-memory index vs. an on-disk index — matches the doc's spec; page-cache effects on a real DB file are not measured.

### Open questions
- 0b: release p50 is 44.37 ms, over the "well under 20 ms" design target. Phase 1 as written only fixes comments and adds a 450 ms regression ceiling. Should the doc keep the 20 ms target as a stated goal the scan misses, or drop it?
- 0c: is the production ntfy server (if borg is ever configured with one) ntfy.sh or self-hosted? A self-hosted `keepalive-interval` would change the 135s default.

## Orchestrator amendment after Phase 0

- Doc defect fixed: the `cargo tree ... -i vault | rg test-util` probe (Phase 0 criterion, Phase 23 criterion) printed nothing for the wrong reason, because the bare `vault` spec failed to resolve (Phase 0a evidence, Addendum D). Both criteria now use `-i vault@<version>` via `cargo pkgid -p vault`.
- Phase 0 open question "keep the 20 ms target?": answered by Phase 1's own bullet: state it as the design target, measured release p50 44.37 ms (Addendum D), `otto perf` enforces only the 450 ms regression ceiling.
- Phase 0 open question "which ntfy server?": no live ntfy config exists; the 135s default stands.

## Phase 1: comments that lie, and the budget they claim (F10, F11)
### Design decisions
- `search_vector` doc states the 20 ms scan as the design target, records the measured ~44 ms release p50 (~292 ms debug) at 21K x 384 on the 3.10 GHz 32-thread Intel host, and says `otto perf` enforces only a 450 ms regression ceiling (vault/src/search/vector.rs): the target is not met and the comment must not claim it.
- `perf_search_vector_21k` (vault/tests/perf.rs): in-memory `SearchIndex`, 21,000 notes with one 384-dim Summary embedding each (deterministic xorshift unit vectors), one warmup, 20 timed calls, asserts p50 < 450 ms (`SEARCH_VECTOR_P50_CEILING`, hardware in its doc comment). `#[cfg(feature = "vec")]` and `#[ignore]`.
- `otto perf` task (.otto.yml): `cargo test --release -p vault --features vec --test perf -- --ignored --nocapture`; not in `ci`. Also runs the pre-existing ignored `perf_scan_vault_thousand_notes`, as the doc's command does.
- Comment fixes: `vector.rs:28` (dropped "Phase A / A6"), `borg/src/intake.rs` (dropped legacy path, `record_intake`, `record_dlq`, "Phase 2"), `borg/src/pipeline/session.rs` (-> `markdown::tests::render_note_keys_matches_the_writer`), `cortex/src/config.rs` (-> `embed::in_inference_pool`), `borg/src/migrate.rs` (outcome fields feed `MigrateReport::changed` and ledger seeding), `borg/src/notify.rs` (-> `_completed` / `_failed` test names), `cortex/src/daemon.rs` and `cortex/src/opts.rs` (`last_run_at` persists in SQLite; only a never-run index gets a full rebuild).

### Deviations
- None.

### Tradeoffs
- Ran the existing scan perf test alongside the new one (doc's exact command, no name filter) vs. filtering: follows the doc; ~8 ms extra.

### Open questions
- None.

### Probe results
- `otto perf` exit 0: `search_vector(21000 rows x 384 dims) p50 = 44.875149ms, max = 45.590573ms` (matches Addendum D 44.37 ms).
- Break-it: inserted `std::thread::sleep(Duration::from_secs(1))` at the top of `search_vector`; `otto perf` exit 1: `search_vector p50 1.048812446s exceeds the 450ms regression ceiling`. Reverted.
- `rg -n 'enforces the budget|record_intake|record_dlq|embed::inference_pool|changed_count|matches_format_reply\b' borg/src cortex/src vault/src -g '!*tests*'` -> no output, rg exit 1.
- `rg -n 'no persisted' cortex/src` -> no output, rg exit 1.
- `otto ci` exit 0, "All CI checks passed!", 2775 tests passed (sum over 27 result lines), 0 failed.

## Phase 2: coverage measures the workspace (F12)
### Design decisions
- `CARGO_SCOPE: "--workspace --features vec"` added to `.otto.yml` `envs`; `check` (cargo check + clippy), `test`, and `cov` all use `$CARGO_SCOPE` unquoted so it word-splits (`.otto.yml` envs/check/test/cov). One definition, so the three tasks cannot drift apart again. `--all-features` dropped from `cov` (it trips vault's mutually-exclusive `vec-candle`/`vec-fastembed` guard).
- The explanatory comment on why not `--all-features` moved to the env definition; the `check` comment now points at it.
- `.github/workflows/ci.yml` mirror comment no longer cites line numbers (they rotted); it names `CARGO_SCOPE`. The CI commands themselves already spelled the same scope.

### Deviations
- None to the spec. The doc's observed-on-main `rg` probe hit `.otto.yml:61` and `:87`; after the change the only hits are comments (`.otto.yml:17`, `:63`).

### Tradeoffs
- An env var shared by three tasks vs. editing each command inline: one place to change. GitHub CI cannot read otto envs, so `ci.yml` keeps literal flags and a comment naming the source.

### Open questions
- Latent hang, not fixed here (Phase 7 ports `vault::fabric` onto `vault::process`): `vault::fabric::is_available` runs `fabric --version` with stdin inherited. When stdin is a socket/pipe that never closes (my backgrounded `otto cov` inherited the tool's socket), `fabric --version` blocks forever and `cortex fabric::tests::test_is_available_returns_bool` hangs the whole run (observed 37 min, until I killed the `fabric` child). With `otto ... < /dev/null` it does not hang. Phase 7/8 should null stdin there. Confirm that is where you want it.
- `otto cov` took about 46 minutes wall clock including the first instrumented build; no bearing on `ci`.

### Probe output
- `otto cov` exit 0, `finished successfully 45m54s`. `Running unittests` lines (target/llvm-cov-target binaries):
  - `src/lib.rs`: borg, cortex, distillers, oracle, sb, strip_transcripts, vault (one each)
  - `src/main.rs`: sb, strip_transcripts
  - plus borg integration tests, and vault's regression tests (`3 passed`: candle parity, two hybrid retrieval)
- Coverage: Lines 81.3% (64164/78905), Functions 82.5% (6872/8332), Regions 81.9%. Was 6.1% lines on main.
- `rg -n -- '--all-features' .otto.yml`: `.otto.yml:17` and `:63`, both comments.
- `otto ci < /dev/null`: exit 0, "All CI checks passed", 2775 tests passed (sum of `test result: ok` lines).

## Phase 3: doctor checks the patterns that are called (F13)
### Design decisions
- Pure parser `fabric_pattern_findings(list_stdout, weekly_pattern: Option<&str>)` split out of `fabric_default_patterns_findings` (`sb/src/cli/checks.rs`), so it is unit-testable without a fabric binary. The shell half runs `fabric -l`, loads `actions.intel.batch-weekly` via `cortex::config::Config::load` (falls back to `weekly_digest`, the `IntelConfig` default, if cortex config is unreadable; a configured `None` means no fallback finding).
- Required `summarize` is an Error when missing; the configured weekly pattern is a Warn naming it when missing.
- Matching is whole-line (`fabric -l` prints one pattern per line), not substring: names tell the truth, so `summarize_paper` no longer satisfies `summarize`. Pinned by `fabric_patterns_match_whole_lines_not_substrings`.
- Deleted dead `IntelConfig.fabric_patterns` and `on_new_note` (`cortex/src/config.rs`, `cortex/src/testutil.rs`); README.md:40 comment now lists `summarize, weekly_digest`.
- Test `intel_config_tolerates_removed_fabric_patterns_and_on_new_note_keys` (`cortex/src/config/tests.rs`) pins that a deployed cortex.yml still carrying the removed keys parses (IntelConfig has no `deny_unknown_fields`).

### Deviations
- `fabric -l` in doctor now runs with stdin nulled: same hang class as the Phase 2 note, a one-line guard, not the `vault::fabric::is_available` fix (still Phase 7).
- Not in spec: whole-line matching instead of substring (see above).

### Tradeoffs
- Reading the weekly pattern from cortex config in the shell half vs. threading config through `external_binaries_findings`: doctor has no config handle there, and `Config::load` is what the sibling checks use.

### Open questions
- Rollout (Scott's, not done): `~/.config/sb/cortex.yml:96` still has `fabric-patterns: [extract_wisdom, summarize]`. Verified harmless: the field is gone from `IntelConfig`, which has no `deny_unknown_fields`, so the key is silently ignored. Evidence: the new binary's `sb doctor` against the live config prints `[config] cortex: ... (parses as typed Config)`. Delete the line whenever convenient.

### Probe output
- `rg -n 'create_tags|extract_wisdom' --type rust`: prints nothing (exit 1). Was `sb/src/cli/checks.rs:374,378`, `cortex/src/config.rs:730,732`.
- New tests (6 in `sb/src/cli/checks/tests.rs`, 1 in `cortex/src/config/tests.rs`) all pass. They cover: both Ok; no `summarize` -> Error; no weekly -> Warn naming it; non-default `batch-weekly` (`my_weekly`) is the one checked; `None` -> only summarize; whole-line match.
- Break-it runs (recorded, reverted):
  - parser mutated to require `create_tags`, drop the weekly finding, substring match: 5 of 6 sb tests FAILED.
  - parser mutated to substring match only: `fabric_patterns_match_whole_lines_not_substrings` FAILED.
  - `deny_unknown_fields` added to `IntelConfig`: `intel_config_tolerates_removed_fabric_patterns_and_on_new_note_keys` FAILED.
- Live `sb doctor` (desk): `fabric pattern present (summarize)`, `fabric weekly fallback pattern present (weekly_digest)`, both Ok.
- `otto ci < /dev/null`: first run exit 1 on `cargo fmt --check` (fixed with `cargo fmt`); second run exit 0, "All CI checks passed", 2782 tests passed (2775 + 7).

## Phase 4: `unwrap_used` at workspace level (F9)
### Design decisions
- `[workspace.lints.clippy] unwrap_used = "deny"` (Cargo.toml) plus `allow-unwrap-in-tests = true` (clippy.toml): one policy, every crate already opts in via `[lints] workspace = true`, so cortex and oracle now carry the deny with no per-crate line.
- Deleted the seven per-crate `#![deny(clippy::unwrap_used)]` lines: vault/src/lib.rs, borg/src/lib.rs, distillers/src/lib.rs, sb/src/lib.rs, sb/src/main.rs, bin/strip-transcripts/src/lib.rs, bin/strip-transcripts/src/main.rs. Kept every `#![allow(clippy::unwrap_used)]` in test files and the one `#[allow]` at borg/src/pipeline.rs:1194.
- Rewrote the Cargo.toml comment so it states the truth (the old one claimed a workspace deny would fire in tests).

### Deviations
- None. No production unwrap surfaced in cortex or oracle, so no code fixes were needed.

### Tradeoffs
- Kept the existing `#![allow]` in test-helper files vs. deleting them as redundant: the doc says keep them, and files like borg/tests/common/mod.rs are not `#[test]` fns or `#[cfg(test)]` modules, so clippy may not treat them as test code.

### Open questions
- None.

### Probe output
- `cargo clippy --workspace --all-targets --features vec -- -D warnings < /dev/null`: exit 0 on the change.
- Break-it: appended `pub fn planted_unwrap() { let _ = Some(1).unwrap(); }` to cortex/src/lib.rs: clippy exit 101, `help: ... rust-clippy/rust-1.98.0/index.html#unwrap_used`, `note: requested on the command line with -D clippy::unwrap-used`, `error: could not compile cortex (lib)`. Reverted; cortex/src/lib.rs has no diff.
- `otto ci < /dev/null`: exit 0, "All CI checks passed", 2782 tests passed (unchanged from Phase 3).

## Phase 5: no silent skips (F16)
### Design decisions
- ENOTDIR preconditions, no chmod, no skip (`cortex/src/proposals/tests.rs`, `cortex/src/sweep/tests.rs`): a regular file used as a parent directory makes `stat(<file>/stages)` fail with ENOTDIR, which permission bits (and root's CAP_DAC_OVERRIDE) cannot bypass. Both tests now hit the `metadata` error arm of `read_staged_candidates`; neither needs `#[cfg(unix)]` any more.
- Renamed `an_unreadable_parent_is_an_error_not_a_skip` -> `a_parent_that_is_a_file_is_an_error_not_a_skip`: the old name described the chmod precondition that no longer exists. `test_unreadable_staging_root_errors_and_preserves_the_queue` keeps its name ("unreadable" still true of a root that cannot be stat'ed); its doc comment now names the ENOTDIR precondition.
- Added `a_staging_root_that_is_a_file_is_an_error_not_an_empty_scan` (`cortex/src/proposals/tests.rs`): the old sweep test chmod-000'd the staging dir ITSELF, which covered the `read_dir` error arm (on non-root hosts). Moving that test to a parent-file precondition (as the spec's mutation criterion requires) would have dropped that arm's coverage; this test keeps it, root-proof (staging root is a regular file: `metadata` Ok, `read_dir` ENOTDIR).
- `test_extract_frames_synthetic_video` (`borg/src/youtube/tests.rs`): `Command::new("ffmpeg").arg("-version").output().expect(..)` replaces the skip; `ffmpeg` added to the apt line in `.github/workflows/ci.yml`.
- `test_cleanup_orphans_end_to_end` (`borg/src/slides/cleanup/tests.rs`): `#[ignore = "needs rkvr on PATH"]`; `rkvr --version` `.expect(..)` replaces the skip.
- `pool_batch_matches_one_at_a_time_real_model` (`vault/src/embedding/candle/tests.rs`): `#[ignore = "downloads bge-small (~133 MB)"]` replaces the `CANDLE_TESTS_REAL` env gate; module doc updated. No other reference to `CANDLE_TESTS_REAL` exists outside `docs/`.
- `candle_bert_matches_sentence_transformers_reference` (`vault/tests/regression/candle/parity.rs`): `#[ignore = "downloads bge-small (~133 MB)"]`; a missing fixture panics with the regeneration recipe; module doc no longer promises a skip.
- Fixture `vault/tests/fixtures/bge-reference.json` generated by `bin/gen-bge-reference.py` and committed (33 KB; 3 texts, 3 x 384-dim vectors, L2 norms 1.0000000 +/- 7e-8). Generator env: Python 3.11 venv, sentence-transformers 6.1.0, transformers 5.18.0, torch 2.14.1+cpu, model `BAAI/bge-small-en-v1.5` from the HF hub.
- `otto ignored` task (`.otto.yml`): `cargo test $CARGO_SCOPE -- --ignored --exact` with the three full paths (`embedding::candle::tests::pool_batch_matches_one_at_a_time_real_model`, `candle::parity::candle_bert_matches_sentence_transformers_reference`, `slides::cleanup::tests::test_cleanup_orphans_end_to_end`), confirmed with `cargo test ... -- --ignored --list`. Uses `$CARGO_SCOPE` (= the doc's `--workspace --features vec`) so it cannot drift from `test`.
- Deleted the two assertion-free tests: `borg/src/extraction/tests.rs` `test_is_available`; `cortex/src/fabric/tests.rs` (its only test, so the file and the `#[cfg(test)] mod tests;` in `cortex/src/fabric.rs` went with it). `vault/src/fabric/tests.rs` left alone (Phase 7).

### Deviations
- Fixture generated with a hand-made venv, not the doc's `pipx run --spec sentence-transformers python bin/gen-bge-reference.py`. Cause chain: the first `pipx run` failed with `[Errno 28] No space left on device` because pip unpacks into `$TMPDIR` (sandbox tmpfs, 16G, 89% full, 1.9G free; torch's default CUDA wheels need more). That failed install left a cached pipx venv (`~/.cache/pipx/c75881569c8ea65`) with no packages, and the retry reused it (`ModuleNotFoundError: sentence_transformers`). Rather than delete the cache, I ran `python3.11 -m venv` + `pip install --extra-index-url https://download.pytorch.org/whl/cpu sentence-transformers` with `TMPDIR=~/.cache/sb-phase5-tmp`, then ran the same script with that venv's python. Same script, same model, same output path. Whether the pipx recipe works on a clean cache is not tested here.
- One test added beyond the spec (`a_staging_root_that_is_a_file_is_an_error_not_an_empty_scan`), to keep coverage the old chmod test provided; see Design decisions.

### Tradeoffs
- ENOTDIR on a parent (metadata arm) for the sweep test vs. staging-root-is-a-file (read_dir arm): the spec's mutation criterion targets the metadata arm for both tests, so both use the parent form; the read_dir arm got its own test instead of being dropped.
- `otto ignored` reuses `$CARGO_SCOPE` (builds and filters every workspace test binary, ~30 binaries reporting `0 passed`) vs. `-p vault -p borg` with per-binary flags: one command, one scope, and the exact-name filter keeps it to the three tests.

### Open questions
- `~/.cache/pipx/c75881569c8ea65` is a broken cached venv left by the ENOSPC failure. A plain `pipx run --spec sentence-transformers ...` on desk will reuse it and fail with `ModuleNotFoundError` until pipx expires it (cache entries live 14 days) or it is removed. I did not delete it. Also `~/.cache/sb-phase5-tmp/` (generator venv + logs) is scratch from this phase; remove with `rkvr rmrf` when convenient.
- Should `bin/gen-bge-reference.py`'s docstring recipe pin CPU torch (`PIP_EXTRA_INDEX_URL=https://download.pytorch.org/whl/cpu`)? The default torch pulls multi-GB CUDA wheels, which is what overflowed the tmpfs. Not changed here (out of Phase 5's file list).

### Probe output
- GitHub CI criterion (root in `debian:bookworm`, rewritten tests `ok`, no `skipping` line): UNVERIFIED until the branch's CI runs (no push in this phase). Local evidence that the precondition holds as root: ran the cortex lib test binary under `unshare -r` (uid 0 in a user namespace, CAP_DAC_OVERRIDE over its files). In that shell `chmod 000` on a dir was bypassed (`ls` succeeded: the exact condition that made the old tests skip), and:
  ```
  0
  chmod 000 bypassed (root)
  test proposals::tests::a_parent_that_is_a_file_is_an_error_not_a_skip ... ok
  test proposals::tests::a_staging_root_that_is_a_file_is_an_error_not_an_empty_scan ... ok
  test sweep::tests::test_unreadable_staging_root_errors_and_preserves_the_queue ... ok
  test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 588 filtered out
  ```
  ENOTDIR is a path-resolution error (a non-directory component), not a permission check, so CAP_DAC_OVERRIDE does not apply; the tests carry no skip branch, so they can only pass or fail.
- Mutation break-it: in `read_staged_candidates`, the non-NotFound `metadata` arm changed to `Err(_e) => return Ok(StagedScan::default())`. Result: `a_parent_that_is_a_file_is_an_error_not_a_skip ... FAILED` (proposals/tests.rs:654), `test_unreadable_staging_root_errors_and_preserves_the_queue ... FAILED` (sweep/tests.rs:840), `test result: FAILED. 1 passed; 2 failed` (the passing one is the read_dir-arm test, as expected). Reverted with `git checkout cortex/src/proposals.rs`; no diff.
- Second break-it (read_dir arm): `read_dir` error mapped to `Ok(StagedScan::default())`: `a_staging_root_that_is_a_file_is_an_error_not_an_empty_scan ... FAILED`. Reverted; no diff.
- `otto ignored < /dev/null`: exit 0, `finished successfully`. The three lines: `slides::cleanup::tests::test_cleanup_orphans_end_to_end ... ok`, `embedding::candle::tests::pool_batch_matches_one_at_a_time_real_model ... ok`, `candle::parity::candle_bert_matches_sentence_transformers_reference ... ok`. Summed over every `test result:` line: `3 passed, 0 failed`. Note: cargo prints one `test result` per binary, so `3 passed` is the sum (1 + 1 + 1 across borg lib, vault lib, vault regression), not a single literal line. The bge model was already in `~/.cache/huggingface/hub` (the generator run fetched it), so no download happened during `otto ignored`.
- Fixture: parses, `texts` length 3 == `embeddings` length 3, each 384 dims (jq). The parity test now asserts cos_dist < 1e-3 against it and passes.
- Missing-fixture break-it: fixture moved aside, parity test run with `--ignored --exact`: `FAILED`, `candle parity fixture missing at .../vault/tests/fixtures/bge-reference.json (No such file or directory (os error 2)). Regenerate with ...`. Fixture restored.
- `rg -n -U -i 'eprintln!\(\s*"[^"]*skip' -g '*tests*' -g '**/tests/**' borg cortex vault distillers oracle sb`: only `vault/src/fabric/tests.rs:46` and `:66` (Phase 7).
- `otto ci < /dev/null`: exit 0, "All CI checks passed!", 2778 passed / 0 failed / 8 ignored (sum over `test result` lines). Delta from 2782: +1 new test, -3 now `#[ignore]`d, -2 deleted. Ignored 5 -> 8. No `skipping` in the log.

## Phase 6: `bin/source-lint`, first checks (F9 guard, F16 guard, F20 inline modules, F20 toolchain)
### Design decisions
- Temporary allowlist for the Phase 5 leftover: `bin/source-lint` check 1 carries `-g '!vault/src/fabric/tests.rs'` (exactly that file, with a comment naming Phase 7). Without it the check hits `vault/src/fabric/tests.rs:46,66` and `otto ci` could not be green. **Phase 7 MUST delete that `-g` line** (and its comment) when it moves those tests to `vault::process`; it then guards the whole tree.
- Skip check is multiline (`rg -U -i 'eprintln!\(\s*"[^"]*skip'`) over `-g '*tests*' -g '**/tests/**'`, so production "skipped" status lines are out of scope. A multiline hit prints two lines (`eprintln!(` and the string line), both tagged.
- Inline-module check: `#\[cfg\(test\)\]\s*\n\s*(pub(..)? )?mod \w+\s*\{` over `--type rust`, so `#[cfg(test)] mod tests;` (the sibling-file form) passes.
- Workspace-lints check reads the member list out of root `Cargo.toml` (not a hardcoded list); the crate list for the rg checks is a fixed `CRATES` array that mirrors it (includes `bin/strip-transcripts`).
- `rg` handling: exit 2 if `rg` is not on PATH, and any rg exit >= 2 (bad pattern, unreadable path) also exits 2 naming the command; rg exit 1 (no match) is the clean case. Every rg call has explicit paths.
- Test-module extraction is by body move: `vault/src/tombstone/tests.rs`, `cortex/src/report/tests.rs`, bodies dedented one level, `#[cfg(test)] mod tests;` left in place (matches `vault/src/search.rs:665`). No test content changed.
- `release.yml` `RUST_VERSION` 1.96.0 -> 1.98.0; `ci.yml` comment rewritten to say release.yml must match and source-lint enforces it; `ripgrep` added to the `ci.yml` apt line; `source-lint` task added to `.otto.yml` and to `ci.before` (after `agents-map`); a `Lint source rules` step added beside `bin/agents-map` in `ci.yml`.

### Deviations
- Not in spec: the allowlist entry above, forced by the Phase 5 leftover and the "must exit 0 today" criterion.
- Toolchain violation message is `release.yml:<line>: RUST_VERSION '<x>' differs from ci.yml '<y>'`, naming release.yml (the file expected to change) rather than both.

### Tradeoffs
- Fixed `CRATES` array vs deriving search roots from `members`: rg wants directories, `members` are the same set, and a new crate that is missing from the array is still caught by check 3 (lints) only if listed in `members`; an unlisted new crate's tests would escape checks 1-2. Kept simple; adding a crate means touching both.
- A path-glob allowlist inside the rg call vs post-filtering output: the glob keeps rg's exit codes meaningful.

### Open questions
- None.

### Probe output
- `bin/source-lint` on the change: exit 0, `source-lint: clean`.
- Planted violations (each reverted):
  - skip: new `cortex/src/plant_tests.rs` with `eprintln!(\n "skipping: no thing")`: exit 1, `cortex/src/plant_tests.rs:3:    eprintln!( (test skips itself ...)`.
  - inline module: new `cortex/src/plant_inline.rs` with `#[cfg(test)]\nmod inner {`: exit 1, `cortex/src/plant_inline.rs:1:#[cfg(test)] (inline test module ...)`.
  - lints: removed `[lints]`/`workspace = true` from `oracle/Cargo.toml`: exit 1, `oracle/Cargo.toml:1: missing [lints] workspace = true ...`.
  - toolchain: `release.yml` set to 1.96.0: exit 1, `.github/workflows/release.yml:10: RUST_VERSION '1.96.0' differs from ci.yml '1.98.0'`.
  - no rg: `PATH=/usr/bin:/bin` (rg lives in `~/.cargo/bin`): exit 2, `source-lint: ripgrep (rg) is not on PATH`.
- Allowlist is load-bearing: without the `-g '!...'` line the same rg prints `vault/src/fabric/tests.rs:46` and `:66`.
- `otto ci < /dev/null`: exit 0, `[source-lint] source-lint: clean`, "All CI checks passed!", 2778 passed / 0 failed / 8 ignored (unchanged from Phase 5: tests only moved).

## Phase 7: `vault::process` (F2)
### Design decisions
- `vault::process::run(cmd, stdin, timeout, label) -> Result<Outcome>` (`vault/src/process.rs`): sets all three pipes itself (stdin `Stdio::null()` when `None`, never inherited), `process_group(0)`, one feeder thread and two drain threads (`read_to_end` returning `io::Result`). One deadline bounds BOTH the leader's exit AND end-of-file on every pipe (and the stdin feeder finishing): a leader that exits while a backgrounded grandchild still holds stdout open (the 0d hazard) is `TimedOut` at the deadline and the group is SIGKILLed, rather than a drain blocking for the grandchild's lifetime. The loop polls `try_wait` + `JoinHandle::is_finished` every 10 ms.
- Errors: spawn failure, `try_wait` failure, pipe read error, stdin write error other than `BrokenPipe`, and a panicked drain/feeder thread are each `Err` naming the label. `BrokenPipe` on stdin is `Ok` (the child chose not to read its input; pinned by `a_child_that_ignores_its_stdin_is_not_an_error`).
- Registry: a fixed `[AtomicI32; 1024]` slot array, not a `Mutex<HashSet>`, so the signal handler reads it lock-free (the 0d spike's shape). `Registration` claims a slot by CAS and clears it on drop. A full registry is an `Err` ("refusing to start an unkillable child"), after killing the just-spawned group: fail closed.
- `kill_registered() -> usize` SIGKILLs every registered group; async-signal-safe (atomics + `killpg` only, no logging).
- `install_interrupt_handler()` (vault) installs one `sigaction` handler for SIGINT and SIGTERM that calls `kill_registered()` then `_exit(130)` (`INTERRUPTED_EXIT_CODE`). sb calls it in `main` after `Cli::parse`, unless `Cmd::is_long_running_daemon()` (`sb/src/cli.rs`): `borg daemon --start`, and `cortex daemon` in its foreground mode, which is `--start` OR no mode flag (`cortex::daemon::run`'s `else` branch runs the daemon; the predicate matches that dispatch, not just `d.start`).
- Signal-handling deps: no `ctrlc`/`signal-hook` crate is a direct dep; `signal-hook-registry` is in the lockfile only through tokio. Used `libc::sigaction` directly via the `libc` dep this phase adds anyway: no new crate, no new lockfile entry.
- `libc` added with `cargo add libc -p vault`, which bumped the lockfile 0.2.186 -> 0.2.190; pinned back with `libc = "0.2.186"` + `cargo update -p libc --precise 0.2.186` so the lockfile diff is only the new edge on `vault`.
- `vault::fabric` ported: `run_pattern_with_max_tokens` -> `process::run` (`TimedOut` -> `FabricError::Timeout`, non-zero -> `FabricError::Failed`); `resolve_binary` (`which`) and `is_available` (`--version`) go through `run` with `stdin: None` and a 10 s `PROBE_TIMEOUT` const (local probes, no network; `is_available` had no timeout before). `build_fabric_command` no longer sets stdio. Deleted `wait_with_timeout` and the `ProcessOutput` alias (no other users).
- `borg/src/fabric.rs:14` doc comment pointed at the deleted `vault::fabric::wait_with_timeout`; now names `vault::process::run` (one-line comment fix so this phase does not create an F11-class stale reference; the borg copy itself is Phase 8).
- Tests (`vault/src/process/tests.rs`, all `#[serial(process)]` because `kill_registered` would kill a sibling test's child; every vault test that spawns, including the fabric ones that resolve a binary, holds the same key): 1 MiB stdout, 1 MiB stderr, 1 MiB stdin, each < 5 s with a 30 s timeout; timeout kills a backgrounded grandchild (leader waiting, and leader already exited); `kill_registered` kills a live call's grandchild and the call returns promptly with SIGKILL status; a finished call leaves nothing registered; non-zero status + stderr; spawn failure names the label. Liveness is checked by pid and process group via `/proc/<pid>/stat` (state != Z), never by name.
- `ParentStdinIsAnOpenPipe` test guard (`process/tests.rs`, `pub(crate)` so fabric tests share it): dup2s the read end of a pipe whose write end stays open onto fd 0 (the exact condition of the 37-minute `fabric --version` hang). `a_child_reading_stdin_exits_promptly_when_given_none` (`cat`, `stdin: None`) and `fabric::tests::is_available_does_not_hang_on_a_parent_stdin_that_never_closes` (fake fabric doing `cat >/dev/null`) run under it, so they bite regardless of the runner's stdin.
- Fabric port tests (`vault/src/fabric/tests.rs`, fake `#!/bin/sh` fabric in a tempdir): 512 KiB reply returned whole; `sleep 30` past a 1 s budget is `FabricError::Timeout` within 5 s; non-zero exit is `FabricError::Failed` carrying stderr; `is_available` false for a missing and a failing binary.
- SIGINT/SIGTERM harness: `vault/tests/interrupt.rs`, `harness = false` (`[[test]]` in `vault/Cargo.toml`). It re-executes its own binary with `--harness-child <pidfile>`; the child installs the handler and runs `sh -c 'sleep 30 & echo $$ $! > pidfile; wait'` through `run`. The parent sends SIGINT (then, in a second round, SIGTERM), asserts exit code 130, that the grandchild pid is dead, and that no live process has the call's pgid. A `Cleanup` guard SIGKILLs the group and the harness on any failure. `--list` prints the one test name so `cargo test -- --list` does not run it.
- `bin/source-lint`: deleted the temporary `-g '!vault/src/fabric/tests.rs'` allowlist line and its comment; the sh-missing skips are gone (moved tests panic instead: `run` returns `Err` on a missing `sh`, and they `.expect` it).
- `vault/AGENTS.md`: entry point, module-map line (`process.rs` (+`process/`)), and an anti-pattern line (raw `output()`/`try_wait` spawning; go through `vault::process::run`).

### Deviations
- 1 MiB stdin test pipes through `sh -c "cat | tr x x"`, not bare `cat`. Root cause, from `strace`: this host's `cat` is uutils coreutils 0.10.0, which calls `fcntl(1, F_SETPIPE_SZ, 1048576)` (`/proc/sys/fs/pipe-max-size` = 1048576), so exactly 1 MiB fits in an undrained stdout pipe and bare `cat` PASSED against the poll-without-drain loop. `tr` writes into a default 64 KiB pipe. Same class as the doc's bare-`head` splice note.
- The SIGINT/SIGTERM handler lives in vault (`install_interrupt_handler`), not in sb: sb only decides whether to call it. Same effect, correct seam: the handler body must read vault's private registry with async-signal-safe code, and the harness test exercises the exact function sb calls.
- SIGTERM also exits 130 (per the doc), not the conventional 143.
- Not in spec: one deadline also covers end-of-file on the pipes and the stdin feeder, not just the leader's exit (see Design decisions). Without it a leader that exits early leaves `run` blocked for a grandchild's lifetime.
- Dropped a `readlink /proc/self/fd/0` test I first wrote: it passed against an inherited stdin whenever the runner's stdin was itself `/dev/null` (as in `otto ci < /dev/null`), so it did not bite. Replaced by the open-pipe guard tests.

### Tradeoffs
- Polling `try_wait` + `is_finished` every 10 ms vs. a `waitpid` thread + channel: one loop, one deadline, no extra thread; 10 ms is noise beside a subprocess spawn.
- Fixed 1024-slot registry vs. unbounded set: lock-free for the signal handler; a full registry fails the call loudly rather than starting a child Ctrl-C cannot reach.
- `harness = false` self-re-exec test vs. a `[[bin]]` or example target in vault: no new artifact in the library crate, and it runs under plain `cargo test`. It prints its own two `ok` lines rather than a libtest `test result:` line, so it is not in the summed pass count; a failure is a non-zero exit that fails `otto ci`.
- `PROBE_TIMEOUT` (10 s) is a const, not a config key: it bounds local `which` / `--version` probes that do no network work.
- A group id is deregistered after the leader is reaped; a pgid could in principle be reused in that window only after every group member is gone and the pid space wraps. Not guarded.

### Open questions
- None.

### Probe output
- `cargo test -p vault --lib -- process fabric < /dev/null`: 29 passed (12 process incl. the later-dropped readlink test, 17 fabric). Final tree: 11 process tests, all ok.
- `cargo test -p vault --test interrupt`: `interrupt harness: SIGINT ok`, `interrupt harness: SIGTERM ok`.
- Break-it, poll-without-drain swap (temporary `run` that spawns with pipes, feeds stdin from a thread, polls `try_wait` without reading, reads output only after exit): `one_mib_of_stdout_is_drained_in_full ... FAILED`, `one_mib_of_stderr_is_drained_in_full ... FAILED`, `one_mib_of_stdin_round_trips_through_cat ... FAILED` (all three panicked in `exited()`: timed out after 30 s), `test result: FAILED. 0 passed; 3 failed`. (First attempt, with bare `cat`: stdin test `ok`; root cause above.) Reverted.
- Break-it, handler without the group kill (`on_interrupt` only `_exit(130)`): `SIGINT: grandchild 583 survived the signal`, harness exit non-zero; the `Cleanup` guard killed the group (`/proc/583` and `/proc/581` absent afterwards, checked unsandboxed). Reverted.
- Break-it, stdin inherited instead of null: `a_child_reading_stdin_exits_promptly_when_given_none ... FAILED` and `is_available_does_not_hang_on_a_parent_stdin_that_never_closes ... FAILED`; `27 passed; 2 failed`. Reverted.
- Break-it, timeout kills only the leader (`child.kill()` instead of `kill_group`): `timeout_kills_a_backgrounded_grandchild ... FAILED` and `timeout_bounds_a_grandchild_holding_the_pipe_after_the_leader_exits ... FAILED`, each `took 30.0s` (drain blocked until `sleep 30` ended). Reverted.
- `rg -n 'fn wait_with_timeout\(' vault/src`: no output, exit 1 (was `vault/src/fabric.rs:187`).
- `rg -n -U -i 'eprintln!\(\s*"[^"]*skip' -g '*tests*' -g '**/tests/**' borg cortex vault distillers oracle sb`: no output, exit 1.
- `bin/source-lint` without the allowlist: exit 0, `source-lint: clean`. `bin/agents-map`: exit 0.
- `otto ci < /dev/null`: exit 0, "All CI checks passed!", 2794 passed / 0 failed / 8 ignored (sum over `test result` lines; +11 process, +5 fabric, -2 moved fabric, +2 sb), plus the harness's `SIGINT ok` / `SIGTERM ok`.

## Phase 8: migrate every spawn site (F2)
### Design decisions
- Each site split into "build the `Command`" + "run it through `vault::process::run`" so tests substitute `sh -c`: `borg/src/fabric.rs` (`transcript_command`/`run_transcript`, `article_command`/`markitdown_command`/`run_article_command`, shared by `fabric -u` and the markitdown fallback), `borg/src/extraction.rs` (`markitdown_command`/`run_extraction`), `borg/src/ocr.rs` (`tesseract_command`/`run_tesseract`), `borg/src/stages/fetcher.rs` (`markitdown_stdin_command`/`run_markitdown`).
- Every timeout is `Err` naming the label. Semantics otherwise preserved: a non-zero exit stays "empty transcript"/"empty OCR text" with a WARN (transcript, tesseract) or `Ok(None)` (article extractors, so fabric -> markitdown -> bail chain is unchanged). `fetch_article_blocking` now WARNs on an Err from either extractor (it was silent on timeout).
- `run_extraction` is `pub(crate)` so the replaced `pipeline/timeouts.rs` test drives production code.
- `pipeline.browser-ua-timeout` (`PipelineConfig.browser_ua_timeout: Duration`, kebab-case key, humantime via the existing `deserialize_humantime`/`serialize_humantime`, default `DEFAULT_BROWSER_UA_TIMEOUT` 30s, `borg/src/config.rs`). Threaded: `PipelineConfig` -> `handlers.rs process_article_jina` -> `jina::fetch_article_markdown(.., browser_ua_timeout)` -> `BrowserUaFetcher::new(markitdown_timeout)`. Commented example added to `config/templates/borg.yml.example`. The fetcher's HTTP call keeps its hardcoded 30 s until Phase 10 reuses the key.
- Deleted `wait_with_timeout` (`borg/src/fabric.rs`), dead `MultiFetcher`/`FabricFetcher`/`JinaFetcher` (and their re-exports in `borg/src/stages.rs`, the `stages/AGENTS.md` line, and the `GitHubFetcher` Fetcher-impl doc comment that named `MultiFetcher`). `GitHubFetcher`'s `Fetcher` impl stays (not in the delete list).
- Replaced the two loop-reimplementing tests (`ocr/tests.rs` `test_ocr_extract_short_timeout_terminates`, `pipeline/timeouts.rs` `test_per_call_timeout_kills_blocking_child` kept by name, body now calls production `run_extraction`). Deleted `multifetcher_falls_back_on_first_failure` (tested a test-local `Chain` struct, subject deleted) and its now-unused `AlwaysFailFetcher`.
- Tests added: per site a 1 MiB stdout test using `head -c 1048576 /dev/zero | tr '\0' a` (transcript, `fabric -u`, markitdown fallback, extraction, tesseract, browser-UA markitdown), a 1 MiB stdin test via `cat | tr x y` for browser-UA, timeout-is-Err tests, non-zero-exit and missing-binary tests, argv tests, config default/parse/reject tests (23 new net).

### Deviations
- `fetch_transcript`/`ocr_extract` timeouts changed from `Ok("")` to `Err`, per the doc's "every timeout becomes Err"; their callers already WARN and degrade (`handlers.rs:102-110`, `:666`, `slides.rs:502`).
- Deleted `multifetcher_falls_back_on_first_failure` (not in the doc's list): its only subject was the deleted `MultiFetcher` shape.

### Tradeoffs
- One shared `run_article_command` for `fabric -u` and markitdown vs two copies: identical handling; the label differs. The 1 MiB test is run once per label so each site has its own.
- `BrowserUaFetcher::new(Duration)` takes the timeout now vs waiting for Phase 10's `Result`-returning constructor: the key is required this phase; Phase 10 only changes the return type.

### Open questions
- None.

### Probe output
- `rg -n 'try_wait' --type rust -g '!vault/src/process.rs' -g '!*tests*' -g '!**/tests/**'`: no output (exit 1).
- `rg -n 'fn wait_with_timeout\(' borg/src`: no output (exit 1).
- Break-it: swapped `vault::process::run` for a poll-without-drain loop (spawn, feed stdin on a thread, `try_wait` poll, read only after exit). All seven 1 MiB tests FAILED (30 s timeout -> Err): `fabric::tests::transcript_one_mib_of_stdout_is_returned_whole`, `fabric::tests::article_fabric_one_mib_of_stdout_is_returned_whole`, `fabric::tests::article_markitdown_fallback_one_mib_of_stdout_is_returned_whole`, `extraction::tests::one_mib_of_stdout_is_returned_whole`, `ocr::tests::tesseract_one_mib_of_stdout_is_returned_whole`, `stages::fetcher::tests::browser_ua_markitdown_one_mib_of_stdout_is_returned_whole`, `stages::fetcher::tests::browser_ua_markitdown_one_mib_of_stdin_round_trips`. Reverted (`vault/src/process.rs` has no diff); with the real `run` they pass in `cargo test --package borg --lib`.
- `otto ci < /dev/null`: exit 0, "All CI checks passed!", 2817 passed / 0 failed (sum over `test result` lines; +23 vs Phase 7).

## Phase 9: one HTTP client constructor, daemon clients (F3, F22)
### Design decisions
- `vault` feature `http` (`dep:reqwest`, same feature set as oracle: `json`, `rustls`, no defaults) gates `vault::http` and `vault::daemon::client`; borg and oracle enable it. `cargo add reqwest -p vault --optional` added the edge; the `Cargo.lock` diff is exactly `humantime` and `reqwest 0.13.5` under `vault` (no version bumps). `humantime` (2.4.0, already in the lock) became a vault dependency because `HotkeyConfig` lives there.
- `vault::http::client(timeout) -> eyre::Result<reqwest::Client>` (`vault/src/http.rs`): total timeout, build error carries context. Seam for Phase 10, stated in the module doc: `builder(Timeouts) -> ClientBuilder` and a `Timeouts` type get added and `client` is re-expressed as `builder(Timeouts::total(t)).build()`; `client`'s signature and callers do not change.
- `hotkey.request-timeout` (`HotkeyConfig.request_timeout`, kebab-case humantime, default `DEFAULT_REQUEST_TIMEOUT` 10s) in `vault/src/daemon.rs`. `deserialize_humantime`/`serialize_humantime` moved from `borg/src/config.rs` to `vault/src/config.rs` (borg re-exports them, no call site changed) so vault can type the key; a bad value is a load error naming `request-timeout`. Commented example added to `config/templates/borg.yml.example`.
- `vault::daemon::client::DaemonClient` (`vault/src/daemon/client.rs`): built from `&HotkeyConfig` + the `server.auth-token` reference; address, bearer token and timeout resolved once. `get(path, query, timeout_override)` / `post_json(path, body)` return the response whatever its status (transport errors are `DaemonError::Unreachable`, `is_connect()` for the "daemon not running" branches); `json(path, resp)` / `ensure_success` check status BEFORE parsing: 401 is `Unauthorized` ("daemon at X rejected the request (401) ... check server.auth-token"), other non-2xx is `Status` with a 200-char body preview, a body that fails or stalls is `Parse`. Query pairs are url-encoded.
- Sites routed through it: `borg::ingest`, `borg::reingest` (client built once outside the loop), `replay::reingest_via_daemon` and `poll_trace_terminal` (now takes `&DaemonClient` instead of host/port), `migrate::reingest_failed` (gains the bearer: F22), `borg::queue::fetch` (keeps its typed `FetchError`, adds a `Client` variant; per-request timeout passed as the override), `oracle::queue::fetch` (signature drops the timeout; it is `view.hotkey.request_timeout`). The three constants are gone; `sb borg queue` passes `config.hotkey.request_timeout`, `sb borg wait` caps each request at `min(config.hotkey.request_timeout, time left)`.
- Test stub: `borg/src/stub.rs` (`#[cfg(test)]`, 127.0.0.1:0, behaviors silent / headers-then-stall / json / token-required / 401), registered in `borg/AGENTS.md`; `vault/AGENTS.md` documents `http.rs` and `daemon/client`.

### Deviations
- `reingest`/`reingest_failed`/replay now classify a non-2xx or unparseable `/ingest` answer per item (event or `Err`) instead of `reingest` aborting the whole run on a parse failure (`?` on the old `response.json()`): `reingest` records `ReingestEntryStatus::Error(msg)` and moves on. Same-seam consequence of the status-first helper; a 401 on every item now reads as 401 on every item rather than killing the run on the first.
- `poll_trace_terminal`: a 200 whose body is not a trace state is still a hard `Err` (as before); a non-2xx and a transport error still warn and keep polling.
- `oracle::queue::fetch` lost its `timeout` parameter; `borg::queue::fetch` keeps its one (needed by `wait`'s `min(.., time left)`).
- The criterion "`sb borg wait` ... existing exit-5 test passes": the existing `sb/tests/wait.rs` runs in `otto ci` and passes unchanged.

### Tradeoffs
- One `DaemonClient` with raw-response `get`/`post_json` plus a separate `json` vs. one `get_json<T>`: `borg::queue` and `oracle::queue` need status-specific meaning (404 = predates `/queue`, `?batch=` 404 = unknown batch) before the generic 401/non-2xx handling.
- Moving `deserialize_humantime` into vault vs. a second copy in vault: one definition (borg re-exports).
- Per-request `timeout()` replaces rather than min()s the client timeout (reqwest semantics); the one caller that tightens (`wait`) already computes the min.

### Open questions
- None.

### Probe output
- `rg -n 'const \w*REQUEST_TIMEOUT' sb/src oracle/src`: no output (exit 1). Was `oracle/src/queue.rs:17`, `sb/src/cli/borg.rs:17` (`QUEUE_REQUEST_TIMEOUT`), `sb/src/cli/borg/wait.rs:19`.
- Per entry point, silent listener AND headers-then-stalled-body, each `< 2x` the timeout (400 ms config): `vault daemon::client` (2), `oracle queue` (silent, stalled), `borg::ingest` (2), `borg::reingest` (silent, reported as the item's Error), `replay::reingest_via_daemon` (2) + `poll_trace_terminal` (stalled), `migrate::reingest_failed` (2, events), `borg::queue::fetch` (2). All pass.
- `hotkey.request-timeout: 1s` is the timeout used (`configured_request_timeout_is_the_timeout_used`): a 2 s stall fails, a 0.5 s delay succeeds. Config parse/default/reject tests in `vault/src/daemon/tests.rs`.
- 401 with a non-JSON body: `a_401_with_a_non_json_body_names_the_401_not_a_parse_failure` (vault), and per-entry-point versions for oracle, `ingest`, replay, `reingest_failed`; messages contain "(401)" and not "parse".
- Token-requiring stub accepts `reingest-failed`'s request (`a_token_requiring_daemon_accepts_the_request`); without a token the same stub yields an `HttpError` event.
- Break-it 1 (`vault::http::client` timeout replaced by `connect_timeout(timeout * 1000)`, i.e. no total timeout): 8 borg tests FAILED (30 s stub stall: ingest x2, reingest, replay x3, `reingest_failed` x2), 2 oracle (`silent_daemon_hits_the_request_timeout`, `headers_then_a_stalled_body_hits_the_request_timeout`), 4 vault (`configured_request_timeout_is_the_timeout_used`, `http::tests::client_enforces_its_total_timeout_on_a_silent_listener`, `a_body_that_stalls...`, `silent_listener_...`). Reverted.
- Break-it 2 (F22: `reingest_failed` builds `DaemonClient::new(&config.hotkey, None)`): `a_token_requiring_daemon_accepts_the_request ... FAILED`; 4 others pass. Reverted.
- `bin/agents-map` first failed (`stub.rs`, `http.rs` undocumented), fixed in the two AGENTS.md files. `cargo fmt` fixed one test line.
- `otto ci < /dev/null`: exit 0, "All CI checks passed!", 2849 passed / 0 failed (sum over `test result` lines; +32 vs Phase 8).

## Phase 10: fetchers, ntfy stream, and the guard (F3)
### Design decisions
- `vault::http` (`vault/src/http.rs`): `enum Timeouts { Total(Duration), Stream { connect, read } }` with `Timeouts::total(d)`; `builder(Timeouts) -> reqwest::ClientBuilder` (Total -> `.timeout`, Stream -> `.connect_timeout` + `.read_timeout`, no total) carries the only `#[allow(clippy::disallowed_methods)]`; `client(timeout)` is now `builder(Timeouts::total(timeout)).build()` with its Phase 9 signature and callers unchanged.
- `clippy.toml` `disallowed-methods`: `reqwest::Client::new`, `reqwest::Client::builder`, `reqwest::ClientBuilder::new`, reason pointing at `vault::http`. No test exemptions.
- Every remaining construction migrated: `youtube.rs` (static `LazyLock` + `.expect` replaced by a per-download `vault::http::client(SUBTITLE_HTTP_CEILING)?`, the same 60 s ceiling, `subtitle-fetch-timeout-secs` wrapper on `send` unchanged), `ocr.rs` (`client`), `readability.rs` (`builder` + UA + redirect), `transcription.rs` (`builder(total(timeout))` + UA), `jina.rs` (`client`), `github.rs` (`builder` + UA), `stages/fetcher.rs` `BrowserUaFetcher` (`builder` + UA + redirect), `ntfy.rs` (`builder(Stream)`).
- `GitHubFetcher::new(timeout) -> Result<Self>`; `with_token` and the `Default` impl deleted (Default could not carry a timeout or a `Result`). Test seam `#[cfg(test)] with_api_base` (the API base was a const). `distill_for_publish_repo` gains a `github_timeout` param (caller passes `config.pipeline.github_timeout`); a construction error is folded into the existing `github-fetch-error` fallback with its existing WARN.
- `BrowserUaFetcher::new(timeout) -> Result<Self>`: `pipeline.browser-ua-timeout` now bounds the HTTP fetch (was a hardcoded 30 s) and, separately, markitdown; field renamed `markitdown_timeout` -> `timeout` because it no longer bounds only markitdown. `jina.rs` propagates with context.
- New key `pipeline.github-timeout` (humantime, default `DEFAULT_GITHUB_TIMEOUT` 30 s) and `ntfy.read-timeout` (humantime, default `DEFAULT_NTFY_READ_TIMEOUT` 135 s = 3 x the 45 s ntfy.sh keepalive, Addendum D). Bad values fail the load naming the key. Both documented in `config/templates/borg.yml.example`.
- ntfy: client built once before the loop; `run` takes `read_timeout` (from `NtfyConfig.read_timeout`, `borg/src/lib.rs`). The `while let Ok(Some(line))` that ended silently on a read error is now a match: `Ok(None)` WARNs "stream ended", `Err(e)` WARNs "read failed (e)", both reconnect through `backoff.wait()`. Only a client build failure ends `run` (as `Err`).
- `TranscriptionClient::new` / `with_groq_url` return `Result` instead of `.expect` on the build: `process_youtube` propagates with `?`; `process_audio_inner` routes the error into its existing "Transcription failed, creating minimal note" WARN path.
- `borg/src/stub.rs` gains `Behavior::NtfyKeepalives { every }` (chunked ndjson: `open`, then `keepalive` every `every`); `HeadersThenStall` serves as the stalled ntfy stream. AGENTS.md entries for `stub.rs` and `http.rs` updated.

### Deviations
- Doc says `client(Timeouts) -> Result<Client>`; `client` keeps `client(timeout: Duration)` per the Phase 9 seam decision (same effect, callers unchanged; `builder(Timeouts)` is the general form).
- ntfy connect timeout is `ntfy.read-timeout` too (no separate key, no hardcoded constant): the doc names only `read-timeout`. A connect slower than the tolerated silence is equally dead.
- `TranscriptionClient::new` returning `Result` is not in the doc's list (`GitHubFetcher::new`, `BrowserUaFetcher::new`); done because the migrated build had an `.expect` that panics in production on a builder failure.
- `distill_for_publish_repo` signature gained a parameter (needed to carry `pipeline.github-timeout`).

### Tradeoffs
- youtube: per-download client vs. keeping a `LazyLock` static: a static cannot return `Result` without an `.expect`; one client build per subtitle download is negligible next to yt-dlp.
- youtube kept the 60 s total ceiling plus the `subtitle-fetch-timeout-secs` wrapper rather than making the per-call value the total: making it the total would turn a header timeout from `Ok(None)` (falls through to audio) into `Err`, a behavior change.
- ntfy tests run the real `ExponentialBackoff` (1 s, 2 s) rather than a backoff seam: Phase 13 owns the backoff constructor, and the doc's criterion is stated in terms of those delays; the stalled test takes about 3.6 s.

### Open questions
- None.

### Probe output
- `rg -n 'Client::new\(\)|falling back to default client' --type rust .`: no output (exit 1). (First run hit only the `vault/src/http.rs:5` doc comment; reworded.) Was 14 lines on main.
- `rg -n 'Client::builder\(\)|ClientBuilder::new' --type rust -g '!vault/src/http.rs' .`: no output (exit 1).
- Planted guard (in `borg/src/jina.rs` `jina_fetch`): `reqwest::Client::new()`, `reqwest::Client::builder()`, `reqwest::ClientBuilder::new()` -> clippy `error: use of a disallowed method` for each, "note: build clients with vault::http::client or vault::http::builder", exit 101. Planted `reqwest::Client::new()` in a `#[test]` in `borg/src/ntfy/tests.rs` -> same error (no test exemption), exit 101. Both reverted. Clean clippy (`--workspace --features vec --all-targets -D warnings`) passes, including crates without a reqwest dependency.
- ntfy stalled stub (read-timeout 300 ms, window 3 x 300 ms + 1 s + 2 s + 500 ms slack): `a_stalled_stream_is_reconnected_through_backoff ... ok` (>= 3 connections). ntfy keepalive stub (read-timeout 600 ms, keepalive every 200 ms, window 1.8 s): `keepalives_inside_the_read_timeout_keep_one_connection ... ok` (exactly 1 connection).
- New timeout tests: `github::tests::timeout::{a_silent_api_hits_the_configured_timeout, a_stalled_body_hits_the_configured_timeout, the_stub_api_answers_through_the_seam}`, `stages::fetcher::tests::http_timeout::{a_silent_origin_hits_browser_ua_timeout, a_stalled_body_hits_browser_ua_timeout}` (400 ms config, each `< 2x`), `vault http::tests::{a_stream_client_has_no_total_timeout, a_stream_client_times_out_a_stalled_read, a_total_client_cuts_a_steady_stream_at_its_total}`, config parse/default/reject tests for both new keys.
- Break-it 1 (ntfy read bound 3600 s, i.e. the old unbounded client; GitHub and browser-UA builders back to the hardcoded 30 s): `a_stalled_stream_is_reconnected_through_backoff FAILED` ("expected the first connection plus 2 reconnects within 4.4s, saw 1"), and all 4 GitHub/browser-UA timeout tests FAILED (30.01 s). Reverted.
- Break-it 2 (ntfy built with `Timeouts::total(read_timeout)`; `Stream` mapped to `.timeout(read)`): `keepalives_inside_the_read_timeout_keep_one_connection FAILED` (`left == right` failed), `a_stream_client_has_no_total_timeout FAILED`. Reverted.
- Break-it 3 (`Stream` sets only `connect_timeout`): `a_stream_client_times_out_a_stalled_read FAILED` (30.01 s). Reverted.
- `git diff Cargo.lock`: empty.
- `otto ci < /dev/null`: exit 0, "All CI checks passed!", 2863 passed / 0 failed / 8 ignored (sum over `test result` lines; +14 vs Phase 9).

## Phase 11: read-modify-write fails closed, panics counted (F4 top tier)
### Design decisions
- Corrupt proposals/bridge file is `Err` naming the path, file untouched: `cortex/src/entities.rs:write_proposals`, `cortex/src/bridge.rs:write_bridge_proposals`. The old `unwrap_or_default` then write destroyed a human's review.
- Gate-0 fails closed (`borg/src/stages/raw.rs:stage_0_init`): a load error builds a reason naming the blocklist path, emits `emit_gate_alert` (stage 0, `DomainBlocklist`, default cooldown), WARNs, and bails. The existing `pipeline.rs` stage-0 branch already maps any `stage_0_init` error to `failed` / `intake-rejected`, so no pipeline change was needed. A missing file is still empty (`Blocklist::from_file`).
- Gate-1 (`run_gate_1`): a load error skips the write-back with a WARN naming the path; the rejection is unchanged. `blocklist_updated` is now the truth (`false` on load failure AND on save failure; before it was hardcoded `true`).
- Backfill per-note body extracted from the `tokio::spawn` closure into `BackfillTask::run` with a `Counters` struct (`cortex/src/summarize.rs`) so the closed-semaphore path is drivable by a test. `JoinError` increments `failed` and WARNs; the closed-semaphore path increments `attempted` and `failed`; checkpoint failure WARNs with the note and checkpoint path.
- `update_wikilinks_batch` returns `Relinked { rewritten, unreadable }` and WARNs per unreadable note, continuing the loop (`cortex/src/naming.rs`). `apply_naming` returns `NamingApplied { written, unreadable }`; `apply_migrate_selected` returns `MigrateApplied { count, unreadable }` (`apply_migrate` keeps `usize`); `apply_classify`, `migrate::run` and `lib::lint` add one Warning violation per unreadable path via `Report::add_unreadable_after_rename` (`cortex/src/report.rs`). "The caller reports them" is realized as report violations.
- `slides.yml` manifest write failure WARNs with the video id and work dir (`borg/src/pipeline/handlers.rs`).
- Test seam: `#[cfg(test)] stages::alert::fired_count(trace)` counts alerts that actually fired (not cooldown-suppressed); the existing alert tests only exercise `should_alert`, no seam counted fires.

### Deviations
- `cortex/Cargo.toml` gains dev-dependency `async-trait = "0.1.89"` (already a `distillers` dependency, same locked version) so the panicking test fabric can implement `FabricCaller`. The `Cargo.lock` diff is one line (`"async-trait"` under cortex); no new crate. `cargo add` initially wanted 0.1.92 plus a new `syn 3`; I pinned the locked 0.1.89 and restored the lock instead.
- Return types of `apply_naming`, `apply_migrate_selected`, `update_wikilinks_batch` changed (the doc says "return them to the three callers"; a struct is how a `Vec<String>`/`usize` return can carry them).

### Tradeoffs
- Report violations (Warning, `fix: None`) vs a new field on `Report`: reuses the existing rendering and JSON shape; the daemon fingerprint uses `applied_paths`, not violations, so it is unaffected.
- Local capturing logger in `borg/src/stages/raw/tests.rs` vs adding a `testing_logger` dependency: no dependency for one WARN assertion; it installs the process logger once and filters on the trace id.

### Open questions
- None.

### Probe output
- `rg -n 'let _ = (save_checkpoint|h\.await|slides::write_manifest)' --type rust`: no output (exit 1). Was `cortex/src/summarize.rs:166,190`, `borg/src/pipeline/handlers.rs:354`.
- Break-it (old behavior restored in `raw.rs` Gate-0 + Gate-1, `entities.rs`/`bridge.rs` `unwrap_or_default`, JoinError ignored, closed-semaphore uncounted, unreadable not collected), 9 new tests FAILED: `corrupt_blocklist_fails_the_capture_as_intake_rejected_in_receipts`, `gate_0_with_a_corrupt_blocklist_rejects_the_url_capture_and_fires_one_alert`, `gate_1_with_a_corrupt_blocklist_still_rejects_and_leaves_the_file_alone`, `write_bridge_proposals_refuses_a_corrupt_file_and_leaves_its_bytes`, `write_proposals_refuses_a_corrupt_file_and_leaves_its_bytes`, `a_task_on_a_closed_semaphore_counts_attempted_and_failed`, `backfill_counts_a_panicking_task_as_failed`, `relink_returns_the_unreadable_note_and_still_rewrites_the_rest`, `apply_naming_reports_the_unreadable_note_and_the_lint_report_names_it`. The tenth new test (`gate_0_with_a_missing_blocklist_lets_the_url_capture_through`) pins behavior that must NOT change, so it passes on both. Sources restored from backup after; all 10 pass.
- File-bytes checks compare `std::fs::read` before/after against the seeded corrupt bytes (proposals, bridge, Gate-1, Gate-0, pipeline).
- Gate-0 receipt test: row `status == "failed"`, `failure_stage == "intake-rejected"`, reason contains the blocklist path, `fired_count(trace) == 1`.
- Not driven end to end: the `lib::lint` / `classify` / `migrate::run` report wiring for unreadable notes (each scans the vault itself, so a note cannot be made unreadable between scan and relink). Covered by `apply_naming` returning the path plus `Report::add_unreadable_after_rename` naming it; the three call sites are one line each.
- `otto ci < /dev/null`: exit 0, "All CI checks passed!", 2873 passed / 0 failed / 8 ignored (sum over `test result` lines; +10 vs Phase 10).

## Phase 12: remaining swallowed errors (F4)
### Design decisions
- WARN-capture seam: the Phase 11 capturing logger (private to `borg/src/stages/raw/tests.rs`) moved to a shared `#[cfg(test)] pub(crate) mod logcapture` (`borg/src/logcapture.rs`, registered in `borg/AGENTS.md`); raw tests now import it. A process can install one logger, so every borg WARN test must share one. cortex gets the same pair in its existing `testutil.rs`; sb keeps a local copy in `bootstrap/migrate/tests.rs` (single user).
- `borg/src/migrate.rs:migrate_one_note`: unparseable frontmatter still returns `Ok(None)` (skipped) but WARNs with the note path and the YAML error.
- `borg/src/stages/artifact.rs:read_raw`: a body read failure WARNs with the trace id only when the body file EXISTS. Image/PDF/audio captures never write a body, so absence is normal and stays silent (a blanket WARN would fire on every non-text capture). Result is still the empty body.
- `borg/src/stages/artifact.rs:trace_matches`: an unreadable envelope WARNs with the trace id and error; the trace is still excluded from the listing.
- `borg/src/lib.rs:reingest`: the `--type` filter closure extracted to `note_has_type(vault_root, entry, type_filter)` so the unreadable-note branch is drivable without a ledger and config; unreadable note WARNs with its path and is excluded (as before).
- `cortex/src/entities.rs:known_slugs`: glossary and canonical-tags load errors each WARN with the path and the consequence. A missing glossary is still silent (`load_glossary` returns default for absent); a missing canonical-tags file now WARNs (it was silently skipped).
- `sb/src/cli/bootstrap/migrate.rs`: marker write extracted to `write_marker(&Path)`, WARN with the marker path on failure (the next run retries, now visibly).
- `vault/src/search/stats.rs:note_quality`: `.optional()?` (rusqlite `OptionalExtension`, precedent `borg/src/receipts.rs`); a NULL `quality` column is read as `Option<String>` and flattened to `None`, so NULL stays "unscored" instead of becoming an error.
- `borg/src/service.rs`: `best_effort(program, args)` helper for the three fire-and-forget service-manager calls (WARNs on spawn error AND non-zero exit); the reinstall path WARNs when the pre-install uninstall fails and installs over it.

### Deviations
- `note_has_type`, `write_marker`, and `best_effort` are new small fns (same effect, correct seam: the doc's line sites were inline closures/statements that cannot be driven by a test).
- `note_quality` additionally flattens a NULL column to `None`; the doc only says `.optional()?`. Without it, `row.get::<String>` on a NULL would have turned the old silent `None` into a new hard error for rows that are merely unscored.
- Non-zero exit of `systemctl disable --now` / `daemon-reload` / `launchctl unload` also WARNs (doc says "WARN"; the old code ignored the status entirely).

### Tradeoffs
- Three copies of a ~25-line WARN capture (borg shared, cortex testutil, sb local) vs. exposing one from `vault` behind a `test-util` feature: the feature is Phase 23's work; a pub test helper in `vault` now would be exactly the F15 flaw. Phase 23 can consolidate.
- `read_raw` WARNs only when the body file exists vs. WARNing on every read error: non-text captures have no body file by design.

### Open questions
- None.

### External-service sites (no test, per the doc)
- `borg/src/discord.rs` (text-attachment spawn, was :244) and (plain-message spawn, was :321): `log::warn!("Discord: reply to channel {channel_id} failed (trace {trace_id}): {e}");`
- `borg/src/telegram.rs:claim_polling_session` (was :120): `log::warn!("telegram: failed to confirm update {} (it may be re-delivered): {e}", last.id.0);`
- `borg/src/service.rs` reinstall (was :62): `log::warn!("service: reinstall could not remove the existing service, installing over it: {e:#}");`
- `borg/src/service.rs` `uninstall_systemd` `disable --now` and `daemon-reload` (was :327, :333) and `uninstall_launchd` `launchctl unload` (was :354), all through `best_effort`: `log::warn!("service: `{program} {}` exited with {status}", args.join(" "))` and `log::warn!("service: could not run `{program} {}`: {e}", args.join(" "))`.

### Probe output
- `rg -n -U 'query_row\([^;]*?\.ok\(\)' vault/src borg/src cortex/src oracle/src`: no output (exit 1). Was `vault/src/search/stats.rs:560`.
- New tests (7 break-it-verified + 3 pins): `migrate::tests::a_note_with_unparseable_frontmatter_is_skipped_with_a_warn_naming_the_path`, `stages::artifact::tests::{an_unreadable_body_warns_with_the_trace_id_and_reads_as_empty, a_trace_with_an_unreadable_envelope_is_excluded_with_a_warn_naming_it, an_absent_body_is_normal_and_does_not_warn}`, `tests::{note_has_type_warns_with_the_path_when_the_note_is_unreadable, note_has_type_matches_the_type_line_in_notes_or_inbox}`, `entities::tests::known_slugs_warns_with_the_path_when_the_canonical_tags_file_is_unreadable`, `cli::bootstrap::migrate::tests::{an_unwritable_marker_warns_with_the_marker_path, a_writable_marker_is_written_without_a_warn}`, `search::tests::group_a::test_note_quality_query_failure_is_an_error_not_none`.
- Break-it (every `log::warn!` in the five touched files demoted to `trace!`; `note_quality` back to `.ok()`), `cargo test --workspace --features vec --lib --no-fail-fast`: the seven WARN/propagation tests FAILED (`a_note_with_unparseable_frontmatter_is_skipped_with_a_warn_naming_the_path`, `a_trace_with_an_unreadable_envelope_is_excluded_with_a_warn_naming_it`, `an_unreadable_body_warns_with_the_trace_id_and_reads_as_empty`, `note_has_type_warns_with_the_path_when_the_note_is_unreadable`, `known_slugs_warns_with_the_path_when_the_canonical_tags_file_is_unreadable`, `an_unwritable_marker_warns_with_the_marker_path`, `test_note_quality_query_failure_is_an_error_not_none`); the three pins pass on both. Sources restored from backup; all pass.
- First `otto ci` failed `clippy::collapsible_if` on the telegram confirm; collapsed into a let-chain.
- `otto ci < /dev/null`: exit 0, "All CI checks passed!", 2883 passed / 0 failed / 8 ignored (sum over `test result` lines; +10 vs Phase 11).

## Phase 13: one backoff (F8)
### Design decisions
- `ExponentialBackoff::new(base, cap)` (`borg/src/backoff.rs`); `Default` and the zero-arg constructor are gone. The four transport loops (telegram, discord, signal, ntfy) call `ExponentialBackoff::reconnect()` = `new(RECONNECT_BASE 1 s, RECONNECT_CAP 30 s)`, today's schedule, so the numbers live in two named consts, not four call sites.
- `next_delay(hint: Option<Duration>)` is pure: a hint is capped at `cap`, otherwise `base * 2^attempt` capped; the attempt count advances either way. `wait(label)` = `next_delay(None)` + log `"{label} in {delay} (attempt n)"` + sleep. All callers pass `"reconnecting"`. Added `attempts()` accessor so ntfy's test can observe the count.
- `retry_after_header(value, now: SystemTime) -> Option<Duration>` in `backoff.rs`: delta-seconds, or HTTP-date via `httpdate::parse_http_date` relative to the injected `now`; a date already past is `Some(0)`; garbage is `None`. Lock diff for `httpdate`: one added edge (`+ "httpdate"` under borg), the already-locked 1.0.3.
- `transcription.rs`: per-call `ExponentialBackoff::new(GROQ_RETRY_BASE 1 s, GROQ_RETRY_CAP 20 s)`; both retry sites sleep `backoff.next_delay(..)`. `retry_backoff` deleted. The cap const is renamed `GROQ_RETRY_CAP` because the criterion requires `GROQ_BACKOFF_CAP` to be gone while the 20 s cap still has to live somewhere.
- ntfy: `connected_at` stamped on connect; `settle_backoff` (-> `reset_if_healthy`) replaces the per-message unconditional `backoff.reset()` and also runs when the stream ends, like the siblings. The old per-message reset let a server that accepts, sends one message and drops pin the backoff at 1 s.

### Deviations
- Doc: "`wait` takes a label"; the Groq loop needs a hinted delay, so it calls `next_delay(hint)` and sleeps itself rather than widening `wait` with a hint parameter (same effect, `wait` stays label-only).
- ntfy test pins `settle_backoff`, the one-line helper both call sites use, not the whole `run` loop: observing a reset through `run` would need a 60 s healthy run, and the backoff has no time seam.

### Tradeoffs
- Free `retry_after_header(value, now)` vs. a method on the backoff: it parses a header and needs no backoff state.
- Existing mock-Groq tests (`Option<u64>` Retry-After) left as is; the HTTP-date form is covered by the `retry_after_header` unit tests with a fixed `now`, not the wall clock.

### Open questions
- None.

### Probe output
- `rg -n 'fn retry_backoff|GROQ_BACKOFF_CAP' borg/src`: no output (exit 1). Was 7 hits on main (`transcription.rs`, `transcription/tests.rs`).
- Break-it (sources restored after each): hint left uncapped -> `a_hint_is_used_and_capped_and_the_attempt_advances` FAILED; HTTP-date branch removed -> `retry_after_header_parses_an_http_date_relative_to_now` and `..._date_in_the_past_is_a_zero_wait` FAILED; attempt not advanced on a hint -> `a_hint_is_used_and_capped_and_the_attempt_advances` FAILED; `settle_backoff` back to unconditional `reset()` -> `a_message_on_a_fresh_connection_does_not_reset_the_backoff` FAILED.
- Schedule pinned by `unhinted_schedule_is_1_2_4_then_capped_at_20` (1, 2, 4, 8, 16, 20, 20).
- `otto ci < /dev/null`: exit 0, "All CI checks passed!", 2888 passed / 0 failed / 8 ignored (sum over `test result` lines; +5 vs Phase 12).

## Phase 14: vault::wikilink (F5)
### Design decisions
- `Resolver` is built in this phase, in `vault/src/wikilink.rs`, beside `parse`: the Data Model places it in `vault::wikilink` and Phase 15 consumes it. No caller changes.
- `vault/src/wikilink.rs:parse(body) -> impl Iterator<Item = WikiLink<'_>>`: a hand scanner, not a regex. Code is found first: `prose_lines` walks lines tracking one open fence (backtick or tilde, 3+ chars, up to 3 leading spaces), and drops fence lines, fenced lines, and 4-space/tab-indented lines. Then `links_in_line` scans each prose line for `[[`...`]]`. Inline backtick spans come from `inline_code_spans` (a run of N opens, the next run of exactly N closes, an unmatched run is literal). A link whose span overlaps a code span is not yielded.
- Fence close follows CommonMark: same character, at least the opening length, nothing but whitespace after it. So a "```rust" line inside a fence does not close it, and a backtick opener whose info string holds a backtick ("```x```") is an inline span, not a fence. An unclosed fence runs to the end of the body.
- A `[` inside the brackets aborts the candidate and scanning restarts one byte on, so `[[a [[b]] c]]` yields only `b`. A link never spans a newline, and a single `]` aborts.
- `build`: split at the first `|` (alias = the rest, so `b|c`), drop one trailing `\` from the reference (table-escaped pipe), split at the first `#` (`^` prefix -> block, else heading). Target, heading, block, and alias are trimmed, and an empty heading/block/alias becomes `None`. Nothing is yielded when the target is empty and there is no heading or block (`[[]]`, `[[|b]]`). `[[#h]]` is yielded with an empty target.
- `stem(target)` is a free fn plus the `WikiLink::stem()` method: last `/` segment, `.md` stripped case-insensitively, lowercased.
- `Resolver::new(paths)` keeps the paths and builds one `HashMap<String, Vec<usize>>` keyed by every component-boundary suffix of each lowercased path minus `.md` (`a/dir/x` -> `a/dir/x`, `dir/x`, `x`). Both of the doc's rules become one hash lookup: the path rule (equals, or ends with `/` + target) and the stem rule (the last suffix is the stem). A duplicated stem keeps every note index. `resolve(target)` lowercases the target and strips `.md`. An empty target resolves to nothing, so same-note links are the caller's to handle.
- `vault/AGENTS.md`: Entry Points bullet, Module Map entry (`wikilink.rs` (+`wikilink/`)), and an anti-pattern bullet against private wikilink regexes.

### Deviations
- Code detection is the doc's list (fences of both kinds, inline spans of any length, indented lines), not a byte-for-byte move of `in_code_context`. That function counts every "```"-prefixed line as a toggle and inline backticks by parity. The new one honours fence character and length and matched backtick runs. The doc asks for exactly these rules ("closing fence same character, at least the opening length", "inline backtick spans of any length"). `in_code_context` itself is untouched; Phase 15/16 move its callers.

### Tradeoffs
- Hand scanner vs. one more regex: a regex cannot express "matching backtick run of the same length" or fence state. The scanner keeps byte spans exact and borrows every field from the body.
- Suffix index (one entry per path component, per note) vs. separate stem and path maps plus an `ends_with` scan: every lookup is O(1) under the SearchIndex mutex, at the cost of `depth` keys per note.
- `parse` collects per line (`Vec` per prose line) vs. a fully lazy iterator: simpler borrow structure, and allocation is bounded by one line's links.

### Open questions
- `sb/tests/wait.rs:wait_closed_port_exits_one` failed once in this phase's first green-candidate `otto ci` (got exit 0, expected 1) and passed on re-run. The test binds 127.0.0.1:0, drops the listener, then dials that port. A sibling test's `start_stub` in the same parallel test binary can be handed the freed ephemeral port and answer it. Unrelated to this phase (no sb or daemon-client code changed). It needs hardening, not retries: hold the listener without accepting, or pick a port no stub can be assigned. Which phase owns it?

### Probe output
- `cargo test -p vault --lib wikilink`: 41 passed (40 in `wikilink::tests` plus one pre-existing `search` test whose name contains "wikilink").
- Table rows -> tests: `bare_link_yields_its_target`, `pipe_splits_target_and_alias`, `hash_yields_a_heading`, `hash_caret_yields_a_block_not_a_heading`, `heading_and_alias_together`, `bang_marks_an_embed`, `table_escaped_pipe_drops_the_backslash_from_the_target`, `path_target_keeps_the_path_and_stems_to_the_last_segment`, `md_suffix_is_kept_in_the_target_and_stripped_from_the_stem`, `heading_only_is_a_same_note_link_with_an_empty_target`, `alias_keeps_every_pipe_after_the_first`, `nested_brackets_yield_only_the_inner_link`, `empty_link_yields_nothing`, `unclosed_link_yields_nothing`, `alias_without_a_target_yields_nothing`, `link_inside_a_backtick_fence_yields_nothing`, `link_inside_a_tilde_fence_yields_nothing`, `link_inside_a_single_backtick_span_yields_nothing`, `link_inside_a_double_backtick_span_yields_nothing`, `link_on_an_indented_line_yields_nothing`, `span_covers_the_brackets`, `embed_span_includes_the_bang`. Resolver: `path_target_matches_equal_and_component_suffix_paths_only` (`dir/x` -> `dir/x.md`, `a/dir/x.md`, not `otherdir/x.md`), `bare_target_matches_by_stem`, `duplicated_stem_resolves_to_every_note_with_it`, `resolution_is_case_insensitive`, plus edge cases.
- Break-it (source restored from a backup after each run): inline-code overlap check disabled -> 3 FAILED (single/double backtick span, triple-backtick inline span). Then five mutations at once: `\` strip removed, fence close length ignored, suffix index cut to full paths only, indented-line check disabled, embed `!` left out of the span -> 10 FAILED, at least one per mutation (`table_escaped_pipe...`, `fence_closes_only_on_same_char_at_least_as_long`, the 6 Resolver stem/suffix tests, `link_on_an_indented_line_yields_nothing`, `embed_span_includes_the_bang`).
- First `otto ci` failed `cargo fmt --check` on two long asserts in `wikilink/tests.rs`, fixed by `cargo fmt --all`. Second failed the unrelated `wait_closed_port_exits_one` (see Open questions). Third: exit 0.
- `otto ci < /dev/null`: exit 0, "All CI checks passed!", 2928 passed / 0 failed / 8 ignored (sum over `test result` lines; +40 vs Phase 13).

## Orchestrator fix after Phase 14: flaky `wait_closed_port_exits_one`

- Observed: Phase 14's second `otto ci` failed `sb/tests/wait.rs:wait_closed_port_exits_one` with exit 0 (expected 1); the re-run passed. No sb code changed in Phase 14.
- Cause (by elimination): exit 0 means `sb borg wait` read a valid drained `/queue` snapshot from the port, and only this binary's `start_stub` serves one. The test bound `127.0.0.1:0`, took the port, and dropped the listener, so a parallel test's `start_stub` (`bind("127.0.0.1:0")`) could be handed the freed port.
- Fix: the test holds a bound, never-listening `socket2::Socket` for its whole run. Proven with a python probe: connect -> "Connection refused"; a second bind -> "Address already in use"; after drop the port is rebindable (the old race window).
- `socket2 = "0.6.3"` added as an sb dev-dependency; already in Cargo.lock, one-line lock diff.
- `otto ci < /dev/null`: exit 0, 2928 passed.
- Separate commit, not part of any numbered phase.

## Phase 15: migrate the readers (F5)
### Design decisions
- `recompute_inbound_link_counts` builds ONE `wikilink::Resolver` from the row paths before the scan, then counts per resolved PATH (`HashMap<&str, u64>`), in `vault/src/search/stats.rs`: a lookup per link under the SearchIndex mutex, and a duplicated stem credits every note with it. Self-links are skipped by path equality, not stem equality.
- `find_inbound_links`: prefilter widened to `LIKE '%{stem}%'`; the verifier is `Resolver::new([path])` over the one target note, so `[[dir/x]]` credits `dir/x.md` only (`vault/src/search/query.rs`).
- `find_outbound_links` and `cortex/src/graph.rs` iterate `wikilink::parse`, skipping empty-target (same-note) links; the stopword check still judges the RAW target before resolution.
- `vault::search::extract_wikilinks`, its regex, and dead `orphan_notes` are deleted; no shim left behind. `vault/src/search/AGENTS.md` bullet updated.
- `cortex/src/quality.rs`: `build_inbound_index` returns the set of linked note PATHS from one Resolver over the note set; `assess_note` looks up the note's own path. No-outbound uses `parse(body).next().is_none()`.
- `cortex/src/links.rs`: resolution is `Resolver` (path/stem), then the lint's own title fallback, then slug-of-target through the Resolver. The title and slug fallbacks stay here only. The private `strip_fenced_code_blocks` and `extract_wikilinks` are gone (`parse` handles code).
- `cortex/src/linking.rs`: `extract_existing_links` yields each link's lowercased stem, so `[[dir/x]]` and `[[x#h]]` now count as already-linked (before: only an exact bare `[[x]]` did).
- `borg/src/dedupe.rs`: `build_link_index` resolves every link once into `resolved path -> linking notes`; `inbound_links(index, tombstone)` reads it (dropped the now-meaningless `stem` parameter). `[[tomb#h]]` and `[[tomb#^b|alias]]` now block the archive.

### Deviations
- `find_outbound_links` and `cortex/src/graph.rs` still resolve through `SearchIndex::resolve_wikilink` / `resolve_note_path` (SQL, with its lenient `LIKE '%target%'` last fallback), not `Resolver`. Same effect on the bug class (heading/code/path links now parse right); building a Resolver per call/per row would need the whole path set per call, and the graph pass resolves per row. Flagged below, not hidden.
- `cortex/src/linking.rs:in_code_context` is untouched: its remaining callers (`inside_structure` in the linker's writer path, `unlink`) are Phase 16.
- Deleted tests that pinned removed private functions: vault `test_extract_wikilinks_{simple,with_alias,with_heading,skips_code_blocks}` (covered by `wikilink::tests`) and cortex `links::tests` `test_extract_wikilinks`, `test_strip_fenced_code_blocks{,_with_language,_preserves_no_fence}`. The links title/slug tests are unchanged and pass.
- Code detection is the stricter `vault::wikilink` one (Phase 14 deviation): a link in inline code or an indented line is no longer counted by quality, linking, dedupe, or inbound counts.
- `cortex/src/linking.rs` compares lowercased link stems, not `Resolver` paths as the Phase 15 bullet says (raised by the implementation audit). Reason: the linker's candidates are note stems, `people`, `projects`, and slugs, and a person or project need not have a note, so there is no path for a `Resolver` to match. Consequence: a dangling `[[missing/rust]]` counts as already linking `rust` and suppresses that suggestion.
- The doc's probe line for `naming.rs` says `:275`; the match is now at `cortex/src/naming.rs:304` (line drift, same content).

### Tradeoffs
- Quality inbound set keyed by path vs. by stem: path keys are what the Resolver returns and fix the `otherdir/x` collision; the cost is a `String` per linked note.
- Quality still counts a self-link as inbound (old behavior, unrequested to change), unlike `recompute_inbound_link_counts` which excludes it.

### Open questions
- Should `find_outbound_links` / the graph wikilink pass drop the `LIKE '%target%'` fuzzy fallback and use `Resolver` (stricter, may mark more links broken)? Not in Phase 15's text; flagged.

### Probe output
- Tests that FAIL on 01408d3 (new tests copied into a throwaway worktree, since removed): `dedupe::tests::run_purge_refuses_a_tombstone_linked_with_a_heading`, `run_purge_ignores_a_link_inside_a_code_span`; `linking::tests::glossary_link_inside_inline_code_is_not_an_existing_link`, `glossary_path_and_heading_links_count_as_already_linked`; `links::tests::test_heading_link_is_not_broken`; `quality::tests::test_path_link_counts_as_inbound_for_the_matching_directory_only`, `test_heading_link_counts_as_inbound`, `test_link_inside_inline_code_is_neither_inbound_nor_outbound`; `search::tests::group_b::find_inbound_links_counts_a_path_link_and_not_another_directory`, `recompute_inbound_link_counts_path_link_credits_only_the_matching_directory`, `recompute_inbound_link_counts_heading_link_and_code_link`. All pass on this phase. Guard tests that pass on both (not bug-pinning): `find_outbound_links_keeps_the_target_without_its_heading_and_skips_same_note_links`, `run_purge_path_link_to_another_directory_does_not_block_the_archive`, `test_path_link_does_not_resolve_to_another_directory`.
- The first draft of `glossary_path_and_heading_links_count_as_already_linked` passed vacuously on the old code (the prose mention sat inside the link, blocked by `in_wikilink`); fixed by adding a separate prose mention, then it failed on old as required.
- `rg -n -F '\[\[' --type rust -g '!*tests*' -g '!**/tests/**' -g '!vault/src/wikilink.rs'` prints exactly `cortex/src/unlink.rs:30` and `cortex/src/naming.rs:304`.
- `otto ci < /dev/null`: exit 0, "All CI checks passed!", 2934 passed (+6 vs Phase 14), 0 failed.

### Phase 15 follow-up
- Deviation 1 withdrawn. `SearchIndex::find_outbound_links` now builds one `Resolver` per call from `SELECT path FROM notes ORDER BY path` and takes the first match. `cortex/src/graph.rs` builds one `Resolver` per pass from the graph rows and `build_edges_for` takes it as a parameter; a bare link to a shared stem now edges to every note with that stem (the Data Model rule). The lenient `LIKE '%target%'` fallback is gone from both paths; `resolve_wikilink` / `resolve_note_path` stay for other callers.
- New tests: `graph::tests::path_wikilink_edges_only_the_matching_directory`, `graph::tests::fuzzy_substring_wikilink_mints_no_edge`, `group_b::find_outbound_links_resolves_a_path_link_to_the_matching_directory_only`, `group_b::find_outbound_links_does_not_fuzzy_match_a_substring`. On eca698a (throwaway worktree, removed) the two fuzzy tests FAIL and the two path tests pass: the old SQL already handled `%/dir/x.md`, so the path tests are guards, and the fuzzy fallback is the only behavior that changed. Blast radius on a realistic fixture was not measured (no cheap fixture; the live vault and index were not touched). The fuzzy test shows the mechanism: 1 link -> 0.
- Deviation 4 (self-link disagreement), with evidence from probes run on 01408d3, eca698a, and the amended tree. Quality: a note `x.md` with body `Self [[x]]` is NOT flagged `no-inbound-links` on 01408d3 and on eca698a (the old inbound set was built from all notes including the source). Unchanged by Phase 15, and it was already different from recompute: PRE-EXISTING difference, recorded and not fixed (out of scope). Recompute: on 01408d3 `notes/x.md` ("Self [[x]]") got 0, and with `a/dup.md` linking `[[dup]]` while `b/dup.md` exists, a note `y` linking `[[dup]]` gave a/dup=1, b/dup=1 (self-skip was by STEM equality, so a's link credited nobody). On eca698a my path-equality self-skip changed that to b/dup=2: a real behavior change. Restored the stem-equality skip; the amended tree prints x=0, a/dup=1, b/dup=1 again. Pinned by `recompute_inbound_link_counts_skips_a_link_sharing_the_source_stem`, which fails on eca698a.
- Note on a stem-equality side effect (pre-existing, kept): a link `[[dir/x]]` from `otherdir/x.md` is also skipped as a "self-link".
- A first `otto ci` of the follow-up failed 6 borg/harvest fixture tests with a fixtures path under `~/.cache/p15-fu`: my throwaway worktree shared the main `target/`, which baked its `CARGO_MANIFEST_DIR` into the borg test binary. Cause confirmed from the panic text; fixed by touching `borg/src/lib.rs` to force a rebuild. Not a flaky test and not a code change. Final `otto ci < /dev/null`: exit 0, 2939 passed (+5 vs the first Phase 15 run).

## Phase 16: migrate the writers and string parsers (F5)
### Design decisions
- `vault/src/wikilink.rs` gains `in_code(body, pos)` (the byte-offset form of the code rules `parse` skips, built on the same `prose_lines` + `inline_code_spans`) and `file_stem(target)` (last segment, `.md` stripped, case kept; `stem()` is now `file_stem().to_lowercase()`). `cortex/src/linking.rs:in_code_context` is deleted; its last caller, the linker's writer guard `inside_structure`, calls `vault::wikilink::in_code`.
- `cortex/src/unlink.rs:retract` iterates `vault::wikilink::parse` and rewrites by `span`; skips heading and block refs explicitly, skips embeds via `link.embed`; code is skipped by `parse` itself. `LINK_RE` is gone. Replacement is the alias, else the target.
- `cortex/src/naming.rs:update_wikilinks_batch`: one `Resolver` over the renamed notes' OLD paths, built once per batch (no regex per stem). `relink` replaces only the target's file-stem bytes inside each resolving link, so folder prefix, `.md`, `#heading`, `#^block`, `|alias`, and `!` survive byte for byte. `file_links` parses the frontmatter one line at a time with the YAML indent stripped (so a 4-space YAML list item is not read as indented code; 30 live notes carry frontmatter wikilinks, none 4-space indented, measured read-only with `rg`), plus the body through `parse`.
- String parsers, each on `parse` + `file_stem`/`stem`: `vault/src/ledger.rs:parse_note_slug` (first link's file stem, case kept; no link -> the raw cell, as before), `cortex/src/association.rs:related_key` (first link's lowercased stem, so `[[foo#h]]` and `[[notes/foo]]` dedupe against `[[foo]]`), `oracle/src/eval/calc.rs:flatten_wikilinks` (alias, else target; `[[a#h]]` -> `a > h` as Obsidian shows it; `[[#h]]` -> `h`; links in code stay literal), `cortex/src/linking.rs:title_text` (a title that is exactly one wikilink reads as its alias, else its target's file stem; any other title is itself; replaces the inline `trim_start_matches("[[")`).
- `sb cortex graph --rebuild` (`sb/src/cli/cortex.rs:GraphArgs`, clap `conflicts_with = "backfill"`) -> `GraphOpts.rebuild` -> `cortex/src/graph.rs:rebuild`. It derives every note's deterministic edges in memory, then calls the new `vault::search::SearchIndex::replace_edges_of_kinds(&DETERMINISTIC_KINDS, &edges)`: ONE transaction that deletes exactly those kinds and inserts through the same resolve-endpoint-or-skip loop (`insert_edges_in`, now shared with `insert_edges`). `fact` and `bridge` (consolidation's kind, also fact-layer) survive. Watermarks and `last_run_at` are written only after the swap commits. `check_exclusive` refuses both flags at the library level too.
- `replace_edges_of_kinds` refuses, before writing, any edge whose kind is not in the list (it would otherwise outlive the next replace).
- `build` and `rebuild` share one `Pass` struct (rows, buckets, stopwords, Resolver built once); `build`'s behavior is unchanged, including `--backfill` and the first-run full rebuild still clearing every edge (pinned by `a_full_build_still_clears_the_fact_layer_which_is_why_rebuild_exists`).
- Living docs: `cortex/AGENTS.md` (stopword composition root is `graph::Pass::new`; unlink skips heading/block refs; `--rebuild` invariant bullet), `vault/AGENTS.md` (`in_code`, `file_stem`, rewrite by span).

### Deviations
- `cortex/src/naming/tests.rs:relink_preserves_heading_block_and_embed_syntax` pinned `[[old-name^abc123|quote]]` -> `[[new-name^abc123|quote]]`. Obsidian's block ref is `#^id`; `[[old-name^abc]]` targets a note named `old-name^abc`. The case now uses `#^abc123`, and the inverted pin is `relink_leaves_a_caret_without_hash_alone_because_it_names_another_note`.
- Rename rewriting now follows Obsidian's resolution: `[[archive/old-name]]` is no longer rewritten when `notes/old-name.md` is renamed (the old regex rewrote any folder prefix). Same effect for every link that pointed at the renamed note.
- Links in code are no longer rewritten by a rename, retracted by unlink, or flattened by the eval (Resolved Decision "links in code are not links"). Code detection for the linker's writer guard and unlink is now the `vault::wikilink` one: tilde fences are code; an unmatched lone backtick no longer turns the rest of its line into code (`guard_allows_a_mention_after_an_unmatched_backtick`).
- unlink now retracts a table-escaped `[[every\|Every]]` to `Every` (the old regex read the target as `every\` and never matched). Its alias is the parser's trimmed alias, so `[[every| Every ]]` retracts to `Every`, not ` Every `.
- `flatten_wikilinks` renders `![[e]]` as `e` (was `!e`) and `[[a#h]]` as `a > h` (was `a#h`).
- The doc's probe `cargo test --package cortex --features vec -- unlink naming` is a usage error on this tree: "the package 'cortex' does not contain this feature: vec". Ran `cargo test --package cortex --features vault/vec -- unlink naming` and `cargo test --package cortex -- unlink naming` (both 40 passed).
- Doc line refs drifted: the naming regex was at `naming.rs:304`, the linking title strip at `linking.rs:~127` (same content).

### Tradeoffs
- `--rebuild` holds every derived edge in memory before one write transaction vs. streaming per-note inserts inside the transaction: the reads (`semantic_neighbors`, `note_path_exists`) go through `&SearchIndex` while a rusqlite `Transaction` borrows the connection mutably, and the edge set is a few hundred thousand small structs at most.
- The library-level exclusive check is a pure `check_exclusive(&GraphOpts)` tested directly, not through `run`: `Config::oracle_db_path()` is not configurable, so a `run`-level test whose guard regressed would open the live index.
- Frontmatter wikilinks parsed per line vs. leaving frontmatter to `parse(content)`: the latter would skip a 4-space-indented YAML value as indented code and stop relinking it.
- The insert-failure test injects through a SQLite trigger on a second connection, not a code seam: no test-only branch in production code, and it fails mid-transaction after the delete.

### Open questions
- Pre-existing, not changed here: an incremental graph pass calls `delete_edges_by_src(src)` for every stale target, and a `fact` edge's `src` is an entity hub, so a hub note that changes loses its outgoing `fact` edges until the next `--backfill`. Same family as the `--rebuild` decision. Fix in a later phase, or leave?
- `cortex/src/linking.rs:in_wikilink` still finds an enclosing link with `rfind("[[")`/`rfind("]]")` (the writer guard against nested links). Not one of the doc's four string parsers and not a `\[\[` regex, so left as is.

### Probe output
- `rg -n -F '\[\[' --type rust -g '!*tests*' -g '!**/tests/**' -g '!vault/src/wikilink.rs'`: no output (exit 1). Was `cortex/src/unlink.rs:30`, `cortex/src/naming.rs:304`.
- `rg -n 'in_code_context' --type rust .`: no output.
- `cargo test --package cortex --features vault/vec -- unlink naming`: 40 passed, includes `unlink::tests::leaves_heading_and_block_refs_alone` (the doc's `unlink/tests.rs:220`; its case now adds `[[every#^abc]]`).
- `--rebuild` with fact edges: `graph::tests::rebuild_keeps_fact_and_bridge_edges_and_rebuilds_wikilinks` (temp-file index, 2 fact + 1 bridge + 1 stale wikilink seeded): `fact_edges()` equal before/after, fact count 2, bridge count 1, wikilink edges exactly `a -> dir/c`, `a -> b` (stale `b -> a` gone).
- Injected insert failure: `graph::tests::rebuild_with_an_injected_insert_failure_leaves_the_edge_table_byte_identical`: a `BEFORE INSERT` trigger (`RAISE(ABORT)` on the second wikilink, after the delete ran) on a second connection; the full ordered dump of `edges` (all six columns, PK order) is equal before/after; the error names "rolled back" and "injected"; `last_run_at` is not written. Vault-level twin: `search::graph::tests::replace_edges_of_kinds_rolls_back_when_an_insert_fails`.
- String-parser call sites with a heading or path-form link: `ledger::tests::parse_note_slug_reads_the_file_stem_of_heading_and_path_links`, `association::tests::related_key_treats_heading_and_path_forms_as_the_same_link`, `eval::calc::tests::flatten_wikilinks_renders_heading_path_and_embed_links_as_obsidian_shows_them`, `linking::tests::title_text_reads_a_link_title_as_its_alias_or_file_stem`.
- Break-it, old code (throwaway worktree at a6d486c with the new test files copied in and a `title_text` shim holding the old inline strip; own `CARGO_TARGET_DIR` under `~/.cache`; removed after, target cleaned): 13 FAILED: `association::tests::related_key_treats_heading_and_path_forms_as_the_same_link`, `linking::tests::{guard_allows_a_mention_after_an_unmatched_backtick, guard_blocks_tilde_fenced_code, title_text_reads_a_link_title_as_its_alias_or_file_stem}`, `unlink::tests::{leaves_links_in_a_tilde_fence_and_an_indented_line_alone, retracts_a_table_escaped_piped_link_to_its_display_text}`, `naming::tests::{relink_leaves_a_caret_without_hash_alone_because_it_names_another_note, relink_leaves_a_path_link_to_another_directory_alone, relink_leaves_links_inside_code_alone, relink_preserves_heading_block_and_embed_syntax}`, `eval::calc::tests::{flatten_wikilinks_leaves_links_in_code_literal, flatten_wikilinks_renders_heading_path_and_embed_links_as_obsidian_shows_them}`, `ledger::tests::parse_note_slug_reads_the_file_stem_of_heading_and_path_links`. Guards that pass on both: `relink_rewrites_frontmatter_links_including_indented_yaml`, `leaves_heading_and_block_refs_alone`.
- Finding while doing that: the first worktree run reported all green because the separate target dir had first been populated from the MAIN tree (a cwd reset ran the first command there); cargo then judged the worktree build fresh and ran main's binaries. Cleaned the target dir and reran; the results above are from a clean build of the worktree.
- Break-it, mutations in place (sources restored from scratchpad backups after each): `rebuild` on `clear_edges` + `insert_edges` (the full-build mechanism) -> `rebuild_keeps_fact_and_bridge_edges_and_rebuilds_wikilinks` and `rebuild_with_an_injected_insert_failure_leaves_the_edge_table_byte_identical` FAILED. `replace_edges_of_kinds` deleting every edge and committing the delete before the inserts -> `replace_edges_of_kinds_keeps_every_other_kind` and `..._rolls_back_when_an_insert_fails` FAILED. Kind check removed -> `..._refuses_an_edge_of_another_kind_before_writing` FAILED. `in_code` swapped for the old `in_code_context` body -> `in_code_matches_what_parse_skips`, `in_code_treats_an_unmatched_backtick_as_literal`, `guard_blocks_tilde_fenced_code`, `guard_allows_a_mention_after_an_unmatched_backtick` FAILED. `check_exclusive` disabled -> `rebuild_and_backfill_together_are_refused` FAILED. `sb` `graph_rebuild_parses_into_opts_and_conflicts_with_backfill` cannot exist on the old tree (no `--rebuild` flag).
- Never touched: the live vault, `~/.local/share/sb`, the oracle index. Every graph test uses `tempfile` or `open_memory`.
- `otto ci < /dev/null`: exit 0, "All CI checks passed!", 2964 passed / 0 failed / 8 ignored (sum over `test result` lines; +25 vs Phase 15).

### Phase 16 follow-up
- Withdraws open question 2. `cortex/src/linking.rs:in_wikilink` (the linker's guard against writing a nested link) was still hand-rolled F5 parsing: `before.rfind("[[")` / `rfind("]]")`. It is now `vault::wikilink::parse(text).any(|link| link.span.contains(&pos))`: a position is inside a link iff a parsed link's span (embed `!` included) contains it.
- Unclosed `[[` semantics: NOT kept. The old function treated everything after an unclosed `[[` as inside a link, for the rest of the note, across lines, and also counted a `[[` inside inline code. Nothing relied on that for safety. A mention after a stray `[[` that gets linked yields a well-formed `[[x]]` that `parse` reads as a link. The one malformed case that could produce `[[x]]]]`, a mention directly before a literal `]]`, is still refused by `is_clean_mention`'s separate `body[end..].starts_with("]]")` check. No existing test pinned the unclosed behavior; the full linking suite (61 pre-existing tests) passes unchanged.
- New tests: `guard_ignores_brackets_inside_inline_code_before_the_mention`, `guard_a_stray_open_bracket_does_not_block_later_lines`, `guard_blocks_a_mention_inside_a_later_link_after_a_closed_one`, `guard_blocks_a_mention_inside_an_embed`.
- Break-it (old body restored in place, then restored from a scratchpad backup): `guard_ignores_brackets_inside_inline_code_before_the_mention` and `guard_a_stray_open_bracket_does_not_block_later_lines` FAILED. The other two are guards and pass on both.
- Probe `rg -n -F '"[["' --type rust -g '!*tests*' -g '!**/tests/**' -g '!vault/src/wikilink.rs' . < /dev/null`: no output (exit 1). I first ran it without a path, and bare `rg` waited on stdin; that stray process was killed, not left running.
- `otto ci < /dev/null`: exit 0, "All CI checks passed!", 2968 passed / 0 failed / 8 ignored (+4 vs the Phase 16 run).

## Orchestrator amendment after Phase 16

- Doc defect fixed: Phase 16 criterion `cargo test --package cortex --features vec -- unlink naming` errors with "the package 'cortex' does not contain this feature: vec" (reproduced by the orchestrator at f21c699). Its "observed on main" note was wrong. The criterion now reads `cargo test --package cortex -- unlink naming`, which Phase 16 ran: 40 passed, including `leaves_heading_and_block_refs_alone`.
- Carried to the finalization checkpoint (pre-existing, out of scope): the incremental graph pass runs `delete_edges_by_src` on each changed note, so a changed entity-hub note loses its outgoing fact edges until the next `--backfill` (Phase 16 open question 1). Also from Phase 15: quality counts a self-link as inbound while `recompute_inbound_link_counts` does not; pre-existing on 01408d3.

## Phase 17: one SQL filter builder (F6)
### Design decisions
- `vault/src/search/filter.rs`: `Filter { sql, params: Vec<rusqlite::types::Value> }` with `new(base_sql, base_params)`, `and_eq`, `and_cmp`, `and_tags(alias, tags, all)`, plus `then(tail)` for the `ORDER BY`/`LIMIT` suffix. Builder-style (consumes and returns `Self`). `and_eq`/`and_cmp` take `Option<&str>` and no-op on `None`, so call sites are a flat chain with no per-filter `if let`. `push_tags_filter` is deleted; its dedup (`dedup_tags`) stays in `query.rs`, shared with `index_one`.
- Every placeholder is a bare `?`, base clauses included (`MATCH ?`, `model_version = ?`, `LIKE ?`), so the params vector order is the textual order. Executed with `rusqlite::params_from_iter(&f.params)`.
- Each of the 7 sites got a pure `pub(super)` builder returning a `Filter` (`search_filter`, `list_notes_filter` in `query.rs`; `tag_search_filter`, `notes_by_creator_filter`, `notes_by_source_domain_filter`, `classify_filter` in `stats.rs`; `vector_filter` in `vector.rs`), so the exact SQL is testable without a DB. `classify_filtered_sql` (a method that built SQL) became the free fn `classify_filter`.
- Tests: `search/filter/tests.rs` (builder unit tests) and `search/tests/filters.rs` (per query: exact SQL and params with all filters set, a prepare/placeholder-count check for every builder, and DB-level positive plus per-filter negative for search, list_notes, creator, source-domain, tag_search, classify_stats).
- Doc comments and `AGENTS.md` mentions of `push_tags_filter` (vault, oracle) renamed to `Filter::and_tags`.

### Deviations
- The doc's "observed on main: 40 lines" for the `param_idx` probe is stale after Phase 15 (which deleted callers and shifted sites); at the start of this phase the probe printed fewer lines across the same 7 sites. It now prints nothing.
- Doc names `vector.rs:220`; located by content (`param_idx`) at `vector.rs:231`. Same site.
- `then` is an addition beyond `and_eq`/`and_cmp`/`and_tags`: the trailing `ORDER BY`/`LIMIT`/`GROUP BY` clauses were `sql.push_str` on the raw string; routing them through `Filter` keeps one object per query.

### Tradeoffs
- Builder consuming `self` vs `&mut self` mutation: a chain reads as the WHERE clause it produces and cannot leave a half-built filter around.
- Pure per-query builder fns vs inlining the chain in each method: a few extra small fns, in exchange for exact-SQL assertions that do not need an indexed DB.

### Open questions
- None.

### Probe output
- `rg -n 'param_idx' vault/src`: no output (exit 1).
- `cargo test -p vault --features vec`: inside `otto ci`, all pass.
- Break-it (sources restored from backups after each run, `--cap-lints warn` so the now-unused params compile): (1) `and_eq("n.status", None)` in `search_filter` and `vector_filter`, `and_cmp("date","<=",None)` in `list_notes_filter` -> 5 FAILED: `list_notes_filter_all_set_has_exact_sql_and_params`, `search_filter_all_set_has_exact_sql_and_params`, `vector_filter_all_set_has_exact_sql_and_params`, `search_all_filters_positive_and_each_filter_negative`, `list_notes_all_filters_positive_and_each_filter_negative`. (2) tags dropped (`and_tags("notes", None, ..)`) in `tag_search_filter`, creator, source-domain, `classify_filter` -> 5 FAILED: `classify_filter_has_exact_sql_with_and_without_group_by`, `creator_and_source_filters_have_exact_sql_and_params`, `tag_search_filter_has_exact_sql_and_params`, `creator_source_and_tag_search_positive_and_negative`, `classify_stats_tag_filter_narrows_every_filtered_count`.
- `otto ci < /dev/null`: exit 0, "All CI checks passed!", 2988 passed / 0 failed.

## Orchestrator note after Phase 17

- `da59d7c docs(handoff): quality review flaws from the rerun comparison` (08:22, between Phase 8 `14a6a21` and Phase 9 `eeb6512`) committed `docs/handoff/quality-review-flaws.md`. No phase report claimed it. Kept: handoffs ride with the work per house convention. The file is superseded by this design doc; resolve its state at finalization.

## Phase 18: golden units (F7)
### Design decisions
- Goldens sit beside the tests as `golden/*.service|timer`, read with `include_str!`: `borg/src/service/golden/{minimal,full}.service`, `cortex/src/daemon/golden/{minimal,full}.service`, `borg/src/harvest/timer/golden/{minimal,full}.service` + `nightly.timer`. Tests are appended to each renderer's existing `tests.rs`. "minimal" is default config; "full" sets env-bootstrap, log-level, and (cortex) rayon threads plus a present config file.
- Seam for ambient state (Phase 19 makes these pure): `XDG_CONFIG_HOME` (cortex `cortex_config().exists()`, harvest `borg_config().exists()`) and `XDG_DATA_HOME` (cortex `xdg_data_dir()`), set by env var, not by touching the real HOME. Cortex: `testutil::lock_env()` + `testutil::EnvGuard` + `#[serial_test::serial(xdg_data_home)]` (the key the existing data-dir test uses). Harvest: the file's existing `ENV_LOCK` mutex (borg has no serial_test dependency; none added).
- Config absent: `XDG_CONFIG_HOME=/golden-xdg-config-home-absent` (fixed, nonexistent). Config present: a tempdir holding `sb/<name>.yml`; its path is replaced by `<XDG_CONFIG_HOME>` in the output before comparing, so the golden is machine-independent. Cortex `XDG_DATA_HOME=/golden-xdg-data-home` (fixed; only joined). Borg's renderer is already pure; inputs are plain args.
- Borg has no timer unit; only service goldens exist for it. Harvest's service has no `[Install]` section (the timer carries it); the golden pins that as-is.
- The goldens pin current quirks Phase 20 changes: borg's ExecStart has no `--config`; cortex `After=default.target`.

### Deviations
- Doc says goldens for "service and timer units (whichever exist per subsystem)"; only harvest has a timer, so 7 goldens, not 9. No renderer code changed.
- Test-side normalization (`<XDG_CONFIG_HOME>` replacement) for the config-present cases: the tempdir path is random, so a byte-exact compare needs it; the alternative (a fixed real path) would write outside the tempdir.

### Tradeoffs
- Env-var seam under a lock vs adding a path parameter to the renderers: the latter is Phase 19's job and the phase is tests-only.
- Hand-maintained goldens with `include_str!` vs a bless-on-env-var helper: no bless mechanism, so a golden changes only by an explicit edit that shows in the diff (Phase 20 edits them by hand).

### Open questions
- None.

### Probe output
- `cargo test --workspace --lib golden` on unmodified renderers: 7 golden tests pass (`golden_borg_service_{minimal,full}`, `golden_harvest_service_{minimal,full}`, `golden_harvest_timer_nightly`, `golden_cortex_service_{minimal,full}`).
- Break-it, one byte changed in each renderer (`Restart=always`->`alwayz` in borg/service.rs, `Persistent=true`->`truf` and the service's `PrivateTmp=true`->`truf` in harvest/timer.rs, `Restart=on-failure`->`failurf` in cortex/daemon.rs), run with `--no-fail-fast`: all 7 goldens FAILED (borg 2, harvest 3, cortex 2). An earlier run with only the timer `Persistent` byte changed failed exactly the timer golden (and borg's, from its own mutation) and left the two harvest service goldens green, so each golden bites on its own unit. Renderer sources restored with `git checkout --` afterward (`git status` shows only test files and golden dirs).
- No flaky test observed: golden set run 4 times (3 plain/mutated runs plus `otto ci`), results deterministic.
- `otto ci < /dev/null`: exit 0, "All CI checks passed!", 2995 passed / 0 failed (+7 vs Phase 17's 2988).
- Never written: the live `~/.config/systemd/user/`; no renderer reads the real HOME in these tests.

## Phase 19: vault::systemd (F7)
### Design decisions
- New ungated `vault/src/systemd.rs` (+`systemd/tests.rs`): `ServiceUnit { description, after, wants, start_limit, unit_type, env_bootstrap, home, path_comment, extra_env, exec_start, restart, hardening, wanted_by }`, `TimerUnit`, `render_service`, `render_timer`, `unit_path(home)`, `EnvBootstrap`. Each crate keeps a thin describer (`borg::service::render_systemd_unit`, `cortex::daemon::render_systemd_unit`, `borg::harvest::timer::render_units`) that builds the spec and calls the shared renderer, so the Phase 18 goldens and existing tests keep their seams.
- Restart is `Option<Restart { policy: Always | OnFailure, sec }>`: borg passes `Always`, cortex `OnFailure`, harvest `None` (oneshot). Not unified, per the doc.
- `Hardening::Minimal { why }`: the harvest unit's hardening comment explains why it skips the strict lockdown; the renderer emits `# Hardening (<why>).` so that reason stays required, not free text pasted elsewhere.
- `unit_path(home)` is the one spelling of the unit PATH (mise shims first); the `mise/shims` probe now hits only `vault/src/systemd.rs`.
- Comments preceding env lines (harvest's 4-line PATH note, cortex's rayon note) are data (`path_comment`, `EnvVar.comment`), rendered one `# ` line per text line, so the goldens stay byte-identical.
- Purity: cortex's caller (`install_systemd_service`) now resolves `<xdg data>/sb` (via `ok_or_else(..)?`, closing the `daemon.rs:886` expect the Phase 20 probe expects gone) and `cortex_config()` when it exists; harvest's `install` resolves `borg_config()` when it exists. Both pass `Option<&Path>` for `--config`. Same `.exists()` semantics as before.
- `EnvBootstrap` derives `Debug, Clone, PartialEq, Eq, Deserialize, Serialize`, kebab-case, `env-file` through `vault::paths::deserialize_tilde_pathbuf`: the union of the two copies' derives (borg's had `Serialize`/`PartialEq`, cortex's didn't), same serde shape. Shape pinned three ways: `vault::systemd::tests::env_bootstrap_deserializes_config_form` (+ snake_case/missing-field rejection), `borg::config::tests::test_env_bootstrap_blocks_deserialize_into_shared_type` (borg.yml `daemon:` and `harvest:` blocks), and the cortex `test_daemon_config_deserialize_rayon_and_bootstrap` now asserting the shared type.
- Golden tests pass inputs directly: config-present cases pass the literal path `<XDG_CONFIG_HOME>/sb/<name>.yml` (the placeholder the golden already carries), cortex passes data dir `/golden-xdg-data-home/sb`. Removed: harvest `ENV_LOCK`, the XDG env mutation and tempdirs, cortex `lock_env`/`EnvGuard`/`serial` on the goldens.
- `harvest::timer::tests::config_flag_goes_before_the_subcommand_not_after` now asserts the exact ExecStart (pure input, no tempdir/env); new `no_config_path_omits_config_flag` is its negative. `service_uses_absolute_binary_and_explicit_path` asserts the full ExecStart, dropping a comment that justified a partial match by env-dependence that no longer exists.
- Cortex `test_render_systemd_unit_readwritepaths_covers_vault_and_data_dir` split: the render half asserts the exact `ReadWritePaths=` line from passed inputs; the "oracle DB sits under `<xdg data>/sb`" half moved to `test_sb_data_dir_contains_oracle_db` (still `XDG_DATA_HOME` under `lock_env` + `serial(xdg_data_home)`, since it tests path resolution, not rendering).
- Living docs: `vault/AGENTS.md` entry point + module map line; `CLAUDE.md` systemd paragraph now names `vault::systemd::render_service` as the renderer (dropped its stale `:240`/`:186` line refs).

### Deviations
- `user_unit_dir` (listed in the doc's Architecture block for `systemd.rs`) is not added in this phase: its only callers are the unit-dir lookups Phase 20 rewrites (borg's `home.join(".config/systemd/user")`, the `expect("xdg_config_dir...")` sites in cortex/daemon.rs and harvest/timer.rs, whose removal is Phase 20's criterion). Adding it here would either leave it dead (`deny(dead_code)`) or pull Phase 20 work forward.
- Data Model lists `Hardening::Minimal` with no payload; it carries `why: String` so the harvest unit's hardening comment renders byte-identical (same effect, correct seam).
- Data Model's "extra env" is split into `path_comment` + `extra_env: Vec<EnvVar { comment, name, value }>` because PATH is always emitted (from `home`) and two units carry a comment above an env line.

### Tradeoffs
- Owned `ServiceUnit` (Strings/PathBufs, `env_bootstrap` cloned from config) vs a borrowed `ServiceUnit<'a>`: rendering runs once per install, and owned fields keep the spec trivially constructible in tests; no lifetimes leak into three crates.
- Thin per-crate describers kept vs callers building `ServiceUnit` inline in `install`: keeps one pure, testable function per unit (goldens target it), and the install fns stay the only places that read ambient state.

### Open questions
- None.

### Probe output
- `git diff --stat 2b3dbd1 -- '**/golden/*'`: empty (goldens unchanged); all 7 goldens pass.
- `rg -n 'mise/shims' --type rust -g '!*tests*' .`: exactly 1 line, `./vault/src/systemd.rs:140`.
- `rg -n 'expect\("xdg_' cortex/src/daemon.rs borg/src/harvest/timer.rs`: 5 lines (`cortex/src/daemon.rs:949,990,1026`, `borg/src/harvest/timer.rs:108,143`), the Phase 20 set; the `:886` data-dir expect is gone.
- Break-it: `PrivateTmp=true` -> `truf` in `vault/src/systemd.rs` (both hardening arms), `--no-fail-fast`: the 6 service goldens FAILED, the timer golden passed (timer has no PrivateTmp). Restored; goldens pass again.
- `bin/agents-map`: all module maps check out.
- First `otto ci` failed only on `cargo fmt` (import order, one long `assert!`); after `cargo fmt --all`, `otto ci < /dev/null`: exit 0, "All CI checks passed!", 3007 passed / 0 failed (+12 vs Phase 18's 2995: vault systemd 9, borg 2, cortex 1). No test failed then passed on re-run.

## Phase 20: unit fixes, HOME consistency, drift check (F7, F18)
### Design decisions
- `vault::systemd::user_unit_dir() -> Result<PathBuf>` (`<xdg config>/systemd/user`, pure half `unit_dir_under`) replaces every unit-dir lookup: borg `write_systemd_unit` + `uninstall_systemd`, cortex install/uninstall/status, harvest install/uninstall. This removes the 5 `expect("xdg_config_dir")` sites (cortex 3, harvest 2) and borg's hardcoded `home.join(".config/systemd/user")` (which ignored `XDG_CONFIG_HOME`).
- `vault::systemd::compare_installed(path, rendered) -> Result<UnitState>` with `UnitState::{NotInstalled, Current, Drifted}`: byte comparison; NotFound is NotInstalled (not drift); any other read error is an error, never "current".
- "Same inputs, same fn" for doctor: each crate exposes the ambient-state resolver `--install` already used, now shared: `borg::service::desired_systemd_unit`, `borg::harvest::timer::desired_units`, `cortex::daemon::desired_systemd_unit`. Install calls it, doctor calls it. Doctor (`sb/src/cli/checks.rs::unit_drift_findings`) covers borg.service, sb-harvest.service, sb-harvest.timer, cortex.service, each only when the unit file exists; a config that does not load skips that crate's units (the config section already reports it). Drift is Warn naming the reinstall command (+ daemon-reload/restart); an unreadable unit or a render failure is Error.
- borg pins `--config <borg.yml>` before `--log-level`/`daemon` (a flag on `sb borg`), only when the file exists, same rule as cortex and harvest. Goldens updated (`<XDG_CONFIG_HOME>/sb/borg.yml` placeholder, as harvest's).
- borg's fabricated vault fallback is gone: `desired_systemd_unit` errors ("vault root does not resolve (set vault.root-path in borg.yml)"). `install_systemd` renders BEFORE `systemctl stop borg`, so a render error cannot leave the daemon stopped.
- `blocklist::default_path() -> Result<PathBuf>`; `entries/remove/clear` and `stages::raw::{stage_0_init, run_gate_1}` use `?`. Test call sites `.unwrap()`.
- Wording fixed in `CLAUDE.md` (Developer-fabricated fallback bullet) and `vault/AGENTS.md` (anti-pattern bullet, entry-point list): passwd fallback, `.expect` for infallible helpers, `ok_or_else(..)?` inside `Result` fns.

### Deviations
- Doc says unit dir "from `vault::paths::xdg_config_dir()`"; it comes from the new `vault::systemd::user_unit_dir()` wrapping it (same effect; also the Phase 19 deferred `user_unit_dir`).
- Doctor's reinstall hint for the harvest units is `sb borg harvest --install` + `daemon-reload` (no restart: oneshot service activated by the timer).
- Drift test seam is the pure `drift_finding(unit, installed_path, rendered, fix)` plus `compare_installed`, so doctor tests take tempdir paths directly and never touch `~/.config/systemd/user/` or mutate env. The XDG env var is mutated only in one borg test (`write_systemd_unit_lands_under_xdg_config_home`, under `TEST_XDG_LOCK`, restored after).

### Tradeoffs
- Drift compares against a render using `std::env::current_exe()`, like `--install`; vs hashing only config-derived lines: byte-exact matches the doc's wording and the doc's criterion, at the cost that running a differently-located `sb` (e.g. `target/debug/sb`) reports drift on every unit.
- Skip a crate's units when its config does not load (config section already errors) vs a second error per unit: avoids duplicate noise.

### Open questions
- None.

### Probe output
- `rg -n 'expect\("xdg_' cortex/src/daemon.rs borg/src/harvest/timer.rs borg/src/blocklist.rs`: no output (rc 1).
- Break-it (`UnitState::Drifted` -> `Current`; `unit_dir_under` joins `xsystemd`; `--config` push removed; vault fallback restored; `--no-fail-fast`): FAILED `golden_borg_service_minimal`, `golden_borg_service_full`, `render_systemd_unit_pins_config_before_daemon`, `desired_systemd_unit_errors_when_vault_does_not_resolve`, `write_systemd_unit_lands_under_xdg_config_home` (borg); `drift_finding_warns_on_one_byte_edit_and_names_reinstall` (sb); `compare_installed_distinguishes_current_drifted_and_absent`, `unit_dir_under_joins_systemd_user` (vault). Restored; all pass. (Fresh/absent drift tests are negatives that pass on both; the break `Drifted`->`Current` is what the positive test bites.)
- Real `sb doctor` (debug build `target/debug/sb`, read-only, in the sandbox): `borg.service`, `sb-harvest.service`, `cortex.service` each reported "installed unit differs from the current render" with the reinstall hint; `sb-harvest.timer` did not. Caveat: `current_exe` was `target/debug/sb`, not `~/.cargo/bin/sb`, so ExecStart differs for that reason alone on every unit; a real drift read needs the installed binary after `otto deploy`. The two `systemctl --user` query errors in that run are the sandbox (no user bus), not a finding. The live cortex.service already carries `--config`, `--vault ...obsidian/` (trailing slash) and `RAYON_NUM_THREADS=12`; no `--install` was run.
- `otto ci < /dev/null`: exit 0, "All CI checks passed!", 3019 passed / 0 failed (+12 vs Phase 19's 3007: vault systemd 4, borg service 4, sb drift 4). No test failed then passed on re-run.

## Phase 21: oracle reads the configured vocabulary, fails loud (F14)
### Design decisions
- `oracle::vocab` (`oracle/src/vocab.rs`): `vocabulary_path(borg_yml) -> Result<PathBuf>`, `load_canonical_tags(path) -> Result<Vec<String>>` (error is `"<path>: <cause>"`), `resolve(borg_yml) -> Result<Vec<String>, String>` (the string is the `vocabulary-error` text). No `warn!`-and-empty path remains.
- One key, through the existing view: `BorgView` gained `tags: TagsView { canonical_path: Option<String> }` (`oracle/src/queue.rs`), read by the same `load_view` chain as the daemon address. Kept raw in the view and tilde-expanded in `vocab::configured_path` (`vault::paths::expand_tilde`).
- Absent vs unreadable: `std::fs::metadata(borg_yml)`; `ErrorKind::NotFound` -> default (`vault::paths::canonical_tags()`), any other error -> `cannot read <borg.yml>: ...`, `Ok` -> `load_view(Some(path))` (explicit load: a parse failure errors with "Failed to load config from <borg.yml>"). Tested with a path beneath a regular file (ENOTDIR, which `Path::exists()` reads as absent), a directory at the borg.yml path, and unparseable YAML.
- Tool contract: `schema_info` -> `tags: null` + `vocabulary-error` (other schema values stay reachable); `tag_brief` -> `known: null` + `vocabulary-error` and no "no such tag" message; a brief with notes stays `known: true` without reading the vocabulary (same lazy read as before).
- Test seam: `OracleMcpServer.borg_config: Option<PathBuf>` + `with_borg_config(PathBuf)` (`None` = `vault::paths::borg_config()`). The tilde fixture tests use `oracle/src/testutil.rs` `HomeGuard` (crate-wide `HOME_LOCK` mutex, sets `$HOME` to a tempdir, restores in Drop), so `~/tags.yml` resolves under the fixture and nothing reads the real `~/.config/sb/` or real HOME contents. The old tests at `server/tests.rs` `schema_info_includes_session_note_type` and `tag_brief_flags_an_unknown_tag` now run through `server_with_vocabulary` (tempdir borg.yml + absolute canonical-path).
- borg: `TagsConfig.canonical_path` is a `PathBuf` with `deserialize_tilde_pathbuf` (`borg/src/config.rs`); `startup::validate_canonical_assets(canonical: &Path)` validates the configured file, `serve_init` passes `config.tags.canonical_path`. Its mapping/patterns checks still use the default locations (the doc scopes only the canonical path).
- doctor: `sb/src/cli/checks/vocab.rs` (`mismatch_finding` pure, `path_mismatch_findings` wrapper) wired into `borg_findings`; a new module because `checks.rs` sat at 1475/1500 lines. Warn names both paths and the fix; `None` (silent) when equal.

### Deviations
- The criterion "the only `vault::paths::canonical_tags()` reference left in oracle is the `BorgView` default" holds in spirit but there are two non-test sites in `vocab.rs`: borg.yml absent, and key unset. Both are "the default"; they cannot be one site without an extra indirection.
- `validate_canonical_assets` gained a parameter (same effect, correct seam: the doc says it validates the configured path).
- `ingest_queue` still calls `load_view(None)` and ignores the `borg_config` test seam: it has no vocabulary dependency, and an explicit-path load treats an absent file as an error, which would change its behavior.

### Tradeoffs
- `Option<PathBuf>` seam on the server vs. a `new()` parameter: 28 existing `OracleMcpServer::new` call sites would all change for two tools' sake.
- `resolve` returns `Result<_, String>`: the tools only ever put the text into JSON, and the string is built once, where the path is known.
- The tilde test mutates `$HOME` under a lock rather than injecting a home-dir seam into `expand_tilde` (a third-party crate reading `dirs::home_dir`).

### Open questions
- None.

### Probe output
- `rg -n 'canonical_tags' oracle/src/server.rs` (was `:498,:653,:1170,:1173` on main): no output.
- Break-it (each reverted after): (1) `vocabulary_path` ignores borg.yml and returns the default: 9 FAILED (`a_tilde_canonical_path_is_expanded_under_the_fixture_home`, `schema_info_lists_a_vocabulary_named_with_a_tilde_path`, `schema_info_reports_null_tags_and_the_error_...`, `tag_brief_reports_null_known_and_the_error_...`, `an_unparseable_borg_yml_fails_both_tools_naming_borg_yml`, three borg.yml error tests, `tag_brief_flags_an_unknown_tag`). (2) `metadata` match replaced by `if !borg_yml.exists()`: `a_borg_yml_that_cannot_be_stat_ed_is_an_error_not_the_default` FAILED. (3) fail-open (`unwrap_or_default`, the old behavior): `an_unparseable_borg_yml_...`, `schema_info_reports_null_tags_...`, `tag_brief_reports_null_known_...` FAILED. (4) tilde attribute removed from borg `canonical_path`: `tags_canonical_path_is_tilde_expanded_at_load` FAILED. (5) `validate_canonical_assets` ignoring its argument: `validates_the_configured_path_not_the_default` FAILED. (6) doctor `mismatch_finding` always `None`: `differing_paths_warn_and_name_both` FAILED.
- `otto ci < /dev/null`: exit 0, 3038 passed / 0 failed (+19 vs Phase 20's 3019).
- FLAKE (first `otto ci` of this phase, not reproduced): `borg pipeline::session::tests::session_replace_merges_prior_tags_into_the_published_note` failed with "Failed to apply receipts PRAGMAs: database is locked" (`borg/src/receipts.rs:138`), in a run where `check`/`clippy` were compiling concurrently. Passed on the next full `otto ci` and on 3 consecutive isolated `cargo test -p borg --lib` runs (1321 passed each). The test holds `harvest::TEST_XDG_LOCK` and uses a tempdir `XDG_DATA_HOME`; nothing in this phase touches receipts or that test's code path (its only edit here is the `canonical_path` type in its config line). Root cause not established; left for the parent.
- The same first run also failed `source-lint` (inline `mod tests` in `checks/vocab.rs`) and `agents-map` (`vocab.rs` undocumented); both fixed, not flakes.

## Phase 22: `limit` above `top_k` (F17)
### Design decisions
- `retrieval_depth(configured, limit) = configured.max(limit)` (`oracle/src/server/pipeline.rs`), applied to: configured vector and bm25 `top_k` in `run_pipeline`; the configured graph arm's seed depth (`K_RRF_INPUT`), seed-fuse cap and graph `top_k` truncate (`pipeline_graph_paths` gained a `limit` param); legacy `mode=hybrid` bm25/vector depth; legacy `graph_dispatch` bm25/vector depth and seed-fuse cap (covers `graph` and `graph-hybrid`). At `limit <= 50` every depth equals the old constant, so results are unchanged.
- `knowledge_search` `limit` description (`oracle/src/tools.rs`) now says upper bound, may return fewer when exclude filters drop stub/short notes, and deepens retrieval above top-k.
- Test seam: `OracleMcpServer.query_embedder: Option<Arc<dyn EmbeddingModel>>` (`oracle/src/server.rs`), used by `vector_paths` instead of `embed_query` when set; builder `with_query_embedder` is `#[cfg(test)]`. Without it no oracle test could run the vector arm without the ~100 MB model.
- Tests (`oracle/src/server/pipeline/tests.rs`): 60 notes all matching `zebra` with varied term frequency, each mock-embedded; `limit_index(neighbors)` optionally adds one unembedded non-matching `wikilink` neighbor per note so the configured-graph test isolates the graph arm.

### Deviations
- The legacy-graph and `graph-hybrid` tests use the no-edge fixture. With edges, the old code's two 50-lists fuse to 100 results at `limit=100`, so that shape cannot show the bug.
- Configured graph is asserted as "60 neighbors" (graph is the only enabled method), not "60 matches".
- Added `graph-hybrid` coverage the doc did not list (it shares `graph_dispatch`).

### Tradeoffs
- Test-only `query_embedder` field on the server vs. a process-global embedder override in vault: the field is per-instance and cannot leak across parallel tests.
- Depth tied to `limit` per call vs. raising the default `top_k`: the doc scopes it to calls that ask for more; default-limit calls keep their cost.

### Open questions
- None.

### Probe output
- Break-it (`retrieval_depth` returns `configured`, i.e. the old behavior): 7 FAILED with the old counts: bm25-only 50, vector-only 50, configured hybrid 56, configured graph 50, legacy hybrid 56, legacy graph 50, legacy graph-hybrid 56 (all expected 60). Hybrid is 56 not 50 because the two 50-deep lists overlap and the union is 56. `limit_10_order_is_unchanged_by_the_depth_change` passed both ways; its golden orders were captured by running the fixture on the pre-change code (printed, then pasted as literals). Restored; all pass.
- `otto ci < /dev/null`: exit 0, 3046 passed / 0 failed (+8 vs Phase 21's 3038).
- FLAKE (first `otto ci` of this phase, load average 55 from the concurrent investigation agent, not reproduced): `sb` `tests/wait.rs` `wait_never_drains_exits_five_at_timeout_with_last_snapshot` ("took 9.404172268s", `sb/tests/wait.rs:255`, assertion `elapsed < 8s`) and `wait_stub_never_responds_exits_five_not_a_hang` ("took 10.111939428s", `:265`) failed on the 8 s wall-clock bound with a 3 s `--timeout`. Passed on the next full `otto ci`. Nothing in this phase touches `sb` or `wait`; the bound is load-sensitive.

## Orchestrator fix after Phase 22: `sb borg wait` timing bounds

- Observed: Phase 22's first `otto ci` (load avg 55) failed `wait_never_drains_exits_five_at_timeout_with_last_snapshot` (9.40 s) and `wait_stub_never_responds_exits_five_not_a_hang` (10.11 s) against `elapsed < 8s` with `--timeout 3s`; both passed on rerun.
- Cause, measured: the observed floor is ~4.8 s. (Corrected by the implementation audit: the poll sleep is capped by the time left, `sb/src/cli/borg/wait.rs:134`, so the 3 s deadline is not deferred to the 4 s poll; the gap above 3 s is process and request overhead, not separately measured.) Isolated runs at load 15-19: 4.67-4.97 s. Under 64 busy loops: 6.40 s and 6.49 s. Only 3.2 s of slack separated the floor from the bound, and the bound sat 2 s under the 10 s default `request-timeout` it was meant to distinguish from. Not a Phase 9 regression: the per-request cap holds (a never-responding stub ends at 4.7 s, not 10 s).
- Fix: the test config sets `hotkey.request-timeout: 60s`, so a request that ignores `min(request-timeout, time left)` runs into the 30 s `PROCESS_CAP`; both asserts use `UNCAPPED_BOUND = 20s`.
- Break-it: `sb/src/cli/borg/wait.rs:92` changed to `config.hotkey.request_timeout` (cap removed): `wait_stub_never_responds_exits_five_not_a_hang` FAILED at the 30 s process cap. Restored, no diff.
- `otto ci < /dev/null`: exit 0, 3046 passed.

## Phase 23: test-only code behind test-util (F15)
### Design decisions
- `test-util = []` feature on `vault` and `distillers`, each with a self dev-dependency (`vault = { path = ".", features = ["test-util"] }`), per Addendum D 0a. Crates that use the gated items list the feature under their own `[dev-dependencies]`: borg (vault + distillers), cortex (vault + distillers), oracle (vault), sb (vault, for the WARN capture). Items gated `#[cfg(any(test, feature = "test-util"))]`: vault `insert_test_note_row`, `insert_test_note_full`, `set_test_capture_note`, `set_test_claims`, `set_test_trace`, `delete_note_for_test`, `MockEmbedder` (struct, both impls, plus `hash64` and `l2_normalize`, which only the mock used without a real backend and tripped `deny(dead_code)` in the release build), `MockReranker` (struct, three impls, and its re-export in `search.rs`); distillers `FakeFabric`.
- `FakeFabric` moved from `distillers/src/fabric.rs` to `distillers/src/fabric/fake.rs`, gated at the `mod fake;` and the `pub use`, instead of a cfg on each of its seven items.
- `#[cfg(test)]` (single-crate use): borg `MemArtifactStore` (+ `MemInner`, `MemTrace`, `missing`, re-export in `stages.rs`), distillers `FakeTagClassifier`, oracle `MockJudge`, borg `MockJudge` (with their re-exports), and `cortex::testutil` (`pub mod testutil` is now `#[cfg(test)]`). The imports each of those left unused are gated the same way.
- Phase 12 carry-over: the three copies of the WARN log-capture helper (`borg/src/logcapture.rs`, the pair in `cortex/src/testutil.rs`, the local `WarnCapture` in `sb/src/cli/bootstrap/migrate/tests.rs`) are now ONE implementation, `vault::capture` (`vault/src/capture.rs`: `install()`, `warns_containing()`), gated by `test-util`. The three copies are deleted, their callers (borg tests, cortex `entities/tests.rs`, sb migrate tests) import `vault::capture`, `borg/AGENTS.md` and `vault/AGENTS.md` updated. One logger per process holds because each crate's test binary links one `vault`. The helper has its own test (`vault/src/capture/tests.rs`).
- `sb cortex embed --use-mock` deleted from all three places (the clap arg in `sb/src/cli/cortex.rs`, `EmbedOpts::use_mock` in `cortex/src/opts.rs`, the branch in `cortex/src/embed.rs`; the `MockEmbedder` import there went with it).
- `group_by_slug` deleted. Its two fixture users (`second_run_is_byte_level_no_op`, `tombstone_write_failure_self_heals_next_run_with_no_duplication`, via the `associate_run` helper) now write notes with `write_session_file_with_trace` sharing one trace, and `associate_run` groups with `group_by_session_identity`. Its four tests were removed; the three uncovered invariants are ported onto `group_by_session_identity` in `cortex/src/association/tests/session_identity.rs`: `a_non_session_note_is_never_a_group_member`, `empty_input_yields_no_groups`, `trace_groups_are_ordered_by_trace_not_scan_order`. The doc comments that linked to it were reworded.

### Deviations
- The doc lists `cortex::testutil` under single-crate `#[cfg(test)]`; it was `pub mod`, so it is now `#[cfg(test)] pub mod testutil` (kept `pub` inside the cfg to avoid renaming every `crate::testutil` path).
- `l2_normalize` and `hash64` in `vault/src/embedding.rs` are gated too, which the doc does not list: required, since `deny(dead_code)` fails the release build otherwise.
- Gated `vault`'s `set_test_capture_note`, `set_test_claims`, `set_test_trace` (the doc says `set_test_*`; these are the three).
- Left as instructed: `OracleMcpServer::with_query_embedder` (`#[cfg(test)]`, Phase 22) and `oracle/src/testutil.rs` (Phase 21).

### Tradeoffs
- One shared `vault::capture` behind `test-util` vs. keeping three single-crate copies: vault is already the common dependency of every user, and the copies were each ~25 lines of the same logger.
- Gating the `FakeFabric` module vs. cfg on each item: one cfg pair instead of seven, and the file reads as the test double it is.
- `group_by_slug` deleted (as the doc says) rather than gated: it had no production caller since the trace-keyed grouping landed.

### Open questions
- None.

### Probe output
- `cargo build --release --workspace`: exit 0 (first try failed on `l2_normalize` dead code; fixed by gating).
- `cargo tree --workspace -e normal,features -i vault@0.15.13 | rg test-util`: prints nothing (rg exit 1). Same probe for `distillers@0.15.13`: nothing.
- Positive control, `-e normal,features,dev -i vault@0.15.13`: `vault feature "test-util"` at line 67 and `distillers feature "test-util"` at line 19. For `distillers` with `dev`: `distillers feature "test-util"`.
- `rg -n group_by_slug --type rust .`: nothing (rg exit 1).
- `otto ci < /dev/null`: exit 0, 3046 passed / 0 failed / 8 ignored (`cargo test --workspace --features vec`, so every gated item is reachable from its tests).
- Earlier `otto ci` of this phase also failed `source-lint` (inline `mod tests` in the new `capture.rs`, moved to `capture/tests.rs`) and, in that same run, FLAKE `cortex sweep::tests::test_scan_proposals_staged_arm_finds_what_the_note_arm_cannot` panicked at `cortex/src/sweep/tests.rs:712`: `receipts schema: SqliteFailure(Error { code: SystemIoFailure, extended_code: 5898 }, Some("disk I/O error"))`. `/tmp` (tmpfs) was at 89% used (15G of 16G) during the run; the test passed in the next full `otto ci`. Not reproduced; nothing in this phase touches `sweep`.

## Orchestrator note after Phase 23

- Phase 23's second `otto ci` failed `cortex sweep::tests::test_scan_proposals_staged_arm_finds_what_the_note_arm_cannot` with SQLite `SystemIoFailure` extended code 5898 (primary 10 SQLITE_IOERR, ext 23). Measured at the time of review: `/tmp` tmpfs 89% (15G of 16G); `$TMPDIR` lives on it. Environmental (a full tmpfs), not a code defect; passed on the next run.
- Consumers measured: other Claude sessions' sandbox tmp dirs (largest 2.9G, 1.5G, 1.5G) and `/tmp/borg-youtube-frames`: 2.6G across 55 frame dirs dated 09-30 onward. That last one is a second-brain defect outside this plan: borg's YouTube slide-frame extraction leaves its per-video frame dirs in /tmp. Carried to the finalization checkpoint. Nothing deleted.

## Phase 24: hotkey and launchd commands (F21, F20 unused params)
### Design decisions
- `borg::service::hotkey_command(exe)` and `render_launchd_plist(exe)` are new pure `pub` fns (the commands were inline in `install_hotkey` / `install_launchd`); the hotkey binds `{exe} borg ingest --clipboard`, the plist ProgramArguments are `{exe} borg daemon --start`. They are pub so the clap test in sb (which owns the CLI definition; borg cannot depend on sb) can reach them.
- `install_hotkey(key)` returns `(command, Option<post_install>)`; `HotkeyOutcome::Installed` gains `command`; the banner prints `Hotkey installed: {key} -> {command}` and the configured `hotkey.host/port` (`hotkey.host/port in borg.yml`).
- `--host`/`--port` removed from `HotkeyArgs` and `HotkeyOpts`; host/port come only from `config.hotkey`.
- Tests: `sb/src/cli/tests.rs` parses the hotkey command and the plist ProgramArguments through `Cli::try_parse_from` and asserts they land on `borg ingest --clipboard` and a long-running `borg daemon --start`; `the_old_hotkey_and_plist_forms_do_not_parse` pins that the 01408d3 forms are not sb commands. `borg/src/service/tests.rs` pins the exact command and plist (including the kept `com.obsidian-borg` label and /tmp log paths).
- Renamed strings: "Stopped/Restarted borg service" (sb/src/cli/borg.rs), "cannot reach the borg daemon at ..." (migrate.rs). Kept: extension id, launchd label/log paths, GNOME dconf path.
- Dropped a stray `let _ = command;` in `install_gnome_keybinding` (command is used).

### Deviations
- `borg/src/lib.rs:920` and `borg/src/tests.rs:180` still say "cannot reach obsidian-borg" (the ingest client error and the test pinning it): outside the doc's listed files and probe scope, left alone.

### Tradeoffs
- New pub fns in borg vs a test inside borg: only sb can parse sb's clap; tuple return from `install_hotkey` vs a struct: two values, one caller.

### Open questions
- None. Rollout (`sb borg hotkey --install` on desk) is Scott's; not run here.

### Probe output
- `rg -n 'obsidian-borg' sb/src borg/src/migrate.rs`: no output (rc 1).
- Break-it (hotkey command and plist args reverted to `ingest --clipboard` / `daemon --start`): `the_hotkey_command_parses_as_a_borg_clipboard_ingest` and `the_launchd_plist_runs_a_command_that_starts_the_borg_daemon` FAILED ("parses" panic at sb/src/cli/tests.rs:6); restored, all pass.
- `otto ci` exit 0, 3051 passed.

### Phase 24 follow-up
- Coordinator ask: the ingest client error `borg/src/lib.rs:920` now reads "cannot reach the borg daemon at ..." (matches migrate.rs); `borg/src/tests.rs:180` (`test_ingest_connection_refused`) pins the new text. This closes the Deviation above.
- Break-it: with the test updated and the old string in lib.rs, `tests::test_ingest_connection_refused` FAILED ("expected connection error message, got: cannot reach obsidian-borg at http://127.0.0.1:19999 - ..."); fixed, passes.
- Extension manifest name/description and temp-dir names left alone (identity / internal).
- FLAKE FINDING: the first `otto ci` after the edit failed 2 tests in borg: `slides::tests::test_segment_filtered_one_kept_is_hero` (`borg/src/slides/tests.rs:778:5: assertion failed: tmp.join("slides").join("slide-001.jpg").exists()`) and `slides::tests::test_segment_two_color_blocks_yields_slides` (`tests.rs:317:5: assertion failed: out_dir.join("slides").join("slide-001.jpg").exists()`). Re-run passed (rc 0, 3051). Unrelated to this change; /tmp tmpfs was 89% full (15G/16G) at the time, unconfirmed as cause.

### Phase 24 orchestrator follow-up

- After the worker's amend (992c41d), three user-visible `obsidian-borg` strings remained in `borg/src/lib.rs`: the daemon start log (:212), `reingest`'s connect bail (:875), and the desktop notification summary (:954). Renamed to "Starting borg daemon", "cannot reach the borg daemon at ...", and "borg: {summary}".
- Guard: the Phase 24 probe widened to `rg -n 'obsidian-borg' sb/src borg/src/migrate.rs borg/src/lib.rs`: prints nothing (rc 1). No new test for the :875 bail: reaching it needs a populated ledger fixture, and `ingest`'s identical message is pinned at `borg/src/tests.rs:180`.
- `otto ci < /dev/null`: exit 0, 3051 passed. Amended into the Phase 24 commit.

## Phase 25: extension options page (F23)
### Design decisions
- Permission origin built from `url.hostname` at `borg/clients/extension/options.js:20`: `url.host` carries the port, and WebExtension match patterns carry none, so `http://desk.lan:8181` never matched. The stored endpoint is still the full trimmed `value` (port kept); the outside-origin refusal still runs through `chrome.permissions.contains` against `extension.origin-patterns`.
- Rust-side guard `staged_options_js_builds_the_origin_from_hostname_not_host` in `borg/tests/stage_produces_valid_extension_dir.rs`: no JS harness exists for the extension and none was added; the test stages the extension and pins `${url.hostname}` present, `${url.host}` absent.

### Deviations
- Doc probe says `options.js:19` and `--to "$TMPDIR/ext"`; the line is 20 and the stage dir was under `~/.cache` (/tmp nearly full). Same effect.

### Tradeoffs
- String-pin test vs a JS unit test: a JS harness would mean a new toolchain, ruled out; the pin catches the regression but does not execute the logic.

### Open questions
- None. Rollout (re-sign, install, lappy options-page save of `http://desk.lan:8181`, capture, outside-origin refusal) is Scott's; sign/install not run.

### Probe output
- `rg -n 'url\.host\}' borg/clients/extension`: no output (rc 1).
- `sb borg extension stage --to ~/.cache/ext-p25`: "staged extension v0.15.13"; staged `options.js:20` uses `url.hostname`. Dir removed via rkvr.
- Break-it (reverted to `url.host`): the new test FAILED; restored, 3 passed.
- `otto ci < /dev/null` exit 0, 3052 passed.

## Phase 26: split `borg/src/lib.rs` (F20)
### Design decisions
- `borg/src/server.rs`: `MAX_UPLOAD_BYTES`, `AppState`, `build_router`, `SubsystemStatus`, `ServerStartup`, `ServerHandle`, `serve_init`. `borg/src/server/transports.rs`: `start_telegram_bot`, `start_discord`, `start_ntfy`, `start_signal` (`-> Result<SubsystemStatus>`, its block has the `?` on the thread spawn). Each takes `&mut JoinSet`, `&Arc<Config>` and the `&Option<Telegram>`/`&Option<Desktop>` it used, and its body is the old `serve_init` block verbatim followed by the status variable as the tail expression; `serve_init` calls the four in the old order.
- `borg/src/client.rs`: `resolve_note_text`, `IngestOutcome`, `note`, `ingest_file`, `resolve_ingest_url`, the `Reingest*` types, `note_has_type`, `reingest`, `ingest`, `HOTKEY_NOTIFY_DISPLAY_MS`, `send_notification`. The two `resolve_*` input resolvers moved too: they are the client half of `ingest`/`note`, and leaving them would keep lib.rs mixed.
- `mod client; mod server;` are private; lib.rs re-exports every pub item (`pub use client::{...}`, `pub use server::{AppState, ServerHandle, ServerStartup, SubsystemStatus, build_router, serve_init}`), so the public paths are exactly what they were. No caller changed: sb (`sb/src/cli/borg.rs`) and `routes.rs` (`crate::AppState`) compile untouched.
- `hotkey` / `HotkeyOutcome` stay in lib.rs (not in the doc's client list; a thin wrapper over `service`).
- Watchdog and sidecar-retention spawns stay inline in `serve_init`, as do the Telegram/Desktop notifier builds: they are not transports (the file is `transports.rs`) and the notifier builds return a notifier plus a status.
- Tests: `borg/src/tests.rs` split by subject into `server/tests.rs` (router, auth, CORS, `ServerHandle::wait`, `constant_time_eq`, `/queue` routes) and `client/tests.rs` (`ingest` connection refused, daemon-client bounds, `reingest`, `note_has_type`). Both used `with_xdg_data_home`, so it moved beside `TEST_XDG_LOCK` in `borg/src/harvest.rs` as `#[cfg(test)] pub(crate)` (one copy, next to the lock it takes); both test files `use crate::harvest::with_xdg_data_home;`.
- `borg::serve` deleted: `rg -n 'borg::serve\b|serve\(' sb borg oracle cortex` hits only `oracle::serve`, `axum::serve`, and the test `stub::serve`; no caller of `borg::serve`.
- Comments that would now lie, updated: `borg/src/service.rs:1-5` module doc (server in `server.rs`, entry points in `client.rs`), `borg/src/signal.rs:9` (gating is in `server::transports::start_signal`). `borg/AGENTS.md`: Entry Points (`serve_init` in `server.rs` + transports, `build_router` at `server.rs:40`, CLI helpers in `client.rs`) and the Core pipeline module map (`server.rs`, `client.rs`).

### Deviations
- Not byte-identical, by necessity: each transport fn adds a signature and a tail expression (`telegram_bot_status`, `discord_status`, `ntfy_status`, `Ok(signal_status)`); `with_xdg_data_home` gains `#[cfg(test)] pub(crate)`; the new files carry their own `use` lines and a module doc. Everything else is verbatim.
- `cargo build -p borg --tests` alone fails on `vault::search` in `borg/src/pipeline/tests.rs:943,967` (feature unification: borg's dev build without the workspace does not enable vault's search feature). Untouched by this phase; the workspace build and `otto ci` are green.

### Tradeoffs
- Private `mod` + root re-exports vs `pub mod server/client`: zero public-API change and no second path to the same item.
- Moving the XDG helper into `harvest.rs` vs a copy per test file: one definition beside the one lock; a copy is the duplicate class this doc removes.

### Open questions
- None.

### Probe output
- `wc -l borg/src/lib.rs`: `120 borg/src/lib.rs` (was 997 at 2006394). New: `server.rs` 295, `server/transports.rs` 173, `client.rs` 479.
- Move proof: a Python check took each original line range of `lib.rs` and `tests.rs` at 2006394 and asserted it appears as one contiguous verbatim block in its destination after `cargo fmt`. All 20 ranges OK: lib 1-66 (attrs + mods) and 85-90 (re-exports) -> lib.rs; 67-69, 71-80, 92-318, 450-482 -> server.rs; 320-343, 345-370, 372-393, 395-448 (telegram bot, discord, ntfy, signal blocks) -> transports.rs; 492-958 -> client.rs; 960-994 (hotkey) -> lib.rs; tests 1-157, 185-228, 247-351 -> server/tests.rs; 158-183, 353-484 -> client/tests.rs; 229-245 -> harvest.rs (with the `pub(crate)` prefix). Lines not carried: blanks, the lib-level `use` lines re-expressed per file, `serve` (484-490), and lib's `mod tests;`.
- `bin/agents-map`: "All AGENTS.md module maps check out".
- `otto ci < /dev/null`: exit 0, "All CI checks passed!", 3052 passed / 0 failed (same as Phase 25: no test lost or added).

## Orchestrator note after Phase 26

- Pre-existing, carried to the checkpoint: `cargo build -p borg --tests` alone fails on `vault::search` at `borg/src/pipeline/tests.rs:943,967`. Those lines date from `6d29c655` (2026-07-05), and `borg/Cargo.toml` on 01408d3 enables only `vault/schemars`, so borg's tests compile only through workspace feature unification. Not introduced by this branch.

## Phase 27: index mtime, CLI split, duplicate helper (F20)
### Design decisions
- `vault/src/search/index.rs`: one private `file_mtime_secs(&Path) -> io::Result<i64>` (the only `fs::metadata` in the file), used by the full reindex and by `index_changed`. A failed read is never folded into mtime 0.
- Full reindex: `index_vault_force` delegates to `index_vault_with_stat(vault_root, force, stat)`, which computes every note's mtime into a `Vec<Option<i64>>` after `scan_vault` and before `BEGIN IMMEDIATE`. `None` (read failed) is WARNed with the path and the note is skipped in the loop: not upserted, and still in `all_paths`, so `remove_stale_notes` does not delete its existing row. The `stat` closure is the test seam (`pub(crate)`), since a real note file cannot be made to fail `metadata` between scan and stat deterministically.
- `index_changed` skips the same way (WARN, `skipped += 1`, `continue`).
- `IndexStats` gains `skipped: u64` (doc comment on the field). Its two consumers print it: `oracle/src/lib.rs` (index-complete and reindex log lines) and `sb/src/cli/oracle.rs` (`Skipped:` line in `sb oracle index`).
- CLI split of `sb/src/cli/borg.rs`, located by content at HEAD (the doc's line numbers are stale): `borg/args.rs` (HELP_TEXT static, `BorgCli`, `Command`, every `*Args`/`*Action` type and their `From` impls), `borg/render.rs` (the 13 `print_*` fns), `borg/tools.rs` (`get_tool_validation_help` and the tool-version helpers). `borg.rs` keeps `impl BorgCli { run }`, the `mod` lines, `pub use args::*` (so `borg::BorgCli`, `borg::Command`, `borg::DaemonArgs` used by `cli.rs` and `logger.rs` are unchanged) and `use render::*`.
- Move proof, method: copied `borg.rs` at f24fba1 to scratch; a Python check took each original range (args 18-406, impl 407-775, render 777-1300, tools 1301-1430), stripped the `pub(super) ` prefix from fn lines, and asserted the range appears as one contiguous verbatim block in its destination file after `cargo fmt`. All four: verbatim.
- `sb --help` proof: a script walked `sb --help` and `sb borg --help` recursively through every listed subcommand (93 invocations, 1207 lines) before the split and after; `diff` of the two captures is empty.
- `human_bytes`: `sb/src/cli/checks.rs` copy deleted; its 9 call sites use `vault::rss::human_bytes`.

### Deviations
- Doc line ranges (23-418, 789-1310, 1311-1440) are stale; seams located by content. `args.rs` is 401 lines, `render.rs` 526, `tools.rs` 132, `borg.rs` 382.
- Visibility on moved fns is `pub(super)` (was private) for `print_*` and `get_tool_validation_help`; that is the only text change inside moved bodies.
- Behavior change from the single `human_bytes`: `sb doctor` byte sizes previously printed one decimal (`1.5 MB`, `512.0 B`); they now print the vault form (`1.50 MB`, `512 B`). The doc says one implementation in `vault::rss`; no test or golden pinned the old doctor text.

### Tradeoffs
- Injected `stat` closure vs a fault-injecting filesystem trait: a closure parameter on a `pub(crate)` fn is the smallest seam; production passes `file_mtime_secs`.
- Skipping the note vs indexing it with the previous row's mtime: skipping keeps the row byte-identical and retries next pass; reusing the old mtime would claim freshness that was never read.

### Open questions
- None.

### Probe output
- `rg -n 'fs::metadata' vault/src/search/index.rs`: one line, `519:    let modified = std::fs::metadata(path)?.modified()?;` (inside `file_mtime_secs`). In `index_vault_with_stat` the `stat(..)` calls (mtimes pass) come before `BEGIN IMMEDIATE`; the only other `file_mtime_secs` references are the delegation in `index_vault_force` (passes it as the closure) and `index_changed` (no transaction).
- `wc -l sb/src/cli/borg.rs`: 382 (was 1430). `rg -n 'fn human_bytes' sb/src`: no output.
- New tests (`search::tests::index_mtime`): `metadata_error_keeps_existing_row_and_counts_skipped` (row title stays `B`, mtime stays 1000, `skipped == 1`, `removed == 0`, `updated == 0`), `metadata_error_on_a_new_note_inserts_nothing`, `real_pass_reports_zero_skipped`.
- Break-it: replaced the WARN-and-`None` arm with `Err(_) => Some(0)` (the old fold-to-0 behavior): 2 of 3 FAILED (`the unreadable note is counted: IndexStats { total_scanned: 2, inserted: 0, updated: 1, unchanged: 1, removed: 0, skipped: 0 }`); restored, 3 pass.
- `otto ci < /dev/null`: first run exit 1 on `cargo fmt` diff in `checks.rs` only (rustfmt rewrapped two lines after the rename); after `cargo fmt`, exit 0, "All CI checks passed!", 3055 passed / 0 failed (3052 + 3 new).

## Orchestrator fix: receipts test isolation

### Cause
- `server::tests::test_ingest_endpoint` and `server::tests::write_route_accepts_correct_token` POST `/ingest`, which reaches `intake::record_received_with_sidecar` -> `receipts::open_default()` before the vault-root check. Neither held `harvest::TEST_XDG_LOCK`, so the path resolved from whatever `XDG_DATA_HOME` was at that instant: a concurrent sandboxed test's tempdir `receipts.db`, or (with `XDG_DATA_HOME` unset, as in plain `otto ci`) the LIVE `~/.local/share/sb/borg/receipts.db`, where they ran PRAGMAs and migrations against the daemon's DB.
- "database is locked" mechanism, measured: it is NOT the `busy_timeout`-after-`journal_mode` order. rusqlite 0.34 sets a 5000 ms busy timeout inside `Connection::open` (`inner_connection.rs:115`, `sqlite3_busy_timeout(db, 5000)`), so the old order already waited. A Rust test with a second connection holding `BEGIN EXCLUSIVE` for 200 ms passed with BOTH orders (no break-it possible, so it was not kept). The real failure is two connections opening the same FRESH (rollback-journal) file at once: both hold SHARED and want EXCLUSIVE for the WAL conversion, and SQLite's deadlock avoidance returns SQLITE_BUSY immediately to one without calling the busy handler. Python probe, 2 openers x 300 fresh files: `journal_mode` first 70/600 locked, `busy_timeout` first 126/600 locked; a holder mid-write on a rollback-mode DB fails the opener in 0.0 s even with timeout 5 s. A DB already in WAL never fails (0 errors in the same probe).

### Fix
- Both tests wrapped in `harvest::with_xdg_data_home(tempdir, ..)` (`borg/src/server/tests.rs`).
- `receipts::set_wal_mode` (`borg/src/receipts.rs`): `PRAGMA journal_mode=WAL` retried on `SQLITE_BUSY`, 10 ms pause, 500 attempts (the 5 s budget the rest of the DB waits under); the winner's conversion persists, so a retry returns at once. Deviation from the brief: the requested `busy_timeout`-first reorder was not made, because it is a no-op under rusqlite (above) and its comment would claim a fix that does not exist.
- Structural guard: `receipts::sandbox` (`borg/src/receipts/sandbox.rs`, `#[cfg(test)]`), a thread-local depth counter with an RAII `!Send` `Entered`. `open_default`, `build_pool`, `default_path`, `open_default_with_path` (every borg fn that resolves the receipts DB from the environment; `rg -n 'receipts_db_path|receipts_dir' borg/src`) call `sandbox::assert_active` under `#[cfg(test)]` and panic naming the fn and the fix. Flag set by: `harvest::with_xdg_data_home` (covers server/client tests), `pipeline::session::tests::XdgSandbox` (field), and the inline set/restore sites in `pipeline/tests.rs`, `harvest/tests.rs`, `harvest/publish/tests.rs` (2), `replay/tests.rs` (2), `stages/raw/tests.rs` (2). `service/tests.rs` holds the lock but redirects `XDG_CONFIG_HOME` only and opens no receipts DB, so it does not set the flag.
- Threads: `#[tokio::test]` bodies run on the test thread via `block_on` (current-thread and multi-thread flavors alike); `tokio::spawn` on the current-thread runtime stays on it. The one cross-thread open is `routes::queue`, which opens the DB in `spawn_blocking`: the first guarded run failed the three already-sandboxed `queue_route_*` tests on `tokio-rt-worker`. Handled with `sandbox::carry()` captured before the spawn and `Carried::enter()` inside the closure (both `#[cfg(test)]` statements); sound because the sandboxed caller awaits the join while holding the lock. `watchdog::run`'s `spawn_blocking(run_once)` is not driven by any test.

### Break-it
- Guard without the test fix (HEAD `server/tests.rs` restored): `server::tests` 10/10 runs `2 failed`, exactly `test_ingest_endpoint` and `write_route_accepts_correct_token`, each `receipts::open_default called outside an XDG sandbox: ...`. With the fix: pass. Full borg lib with guard and no fix: 1321 passed, 2 failed (those two).
- `receipts::tests::concurrent_first_opens_of_a_fresh_db_all_succeed` (100 rounds x 3 threads opening one fresh file): with `WAL_RETRY_ATTEMPTS = 0` (single-shot conversion, the old behavior) FAILED, `66 failed opens, first: "round 0: Failed to apply receipts PRAGMAs: database is locked: Error code 5"` (the flake's exact text); restored, pass.

### Loop
- `CARGO_TARGET_DIR=~/.cache/receipts-fix-target cargo test -p borg --lib --features vault/search --no-run`, binary looped 300x from `borg/` with `XDG_DATA_HOME` at a `~/.cache` scratch dir: "database is locked" 0/300 (baseline 2/300). One unrelated failure, run 250: `queue::tests::fetch_stub::fetch_connect_refused_names_the_address` got `PredatesQueue { addr: "127.0.0.1:46653" }`: the test binds port 0, drops the listener, then expects connection refused, and a concurrent test's stub server bound that ephemeral port in between and answered. Reported, not fixed here.
- `otto ci < /dev/null`: exit 0, "All CI checks passed!", 3056 passed / 0 failed (3055 + 1 new).

## Orchestrator fix: youtube frame-extraction test isolation

### Cause
- `youtube::tests::test_extract_frames_synthetic_video` used a fixed `std::env::temp_dir().join("borg-test-frames-synth")` and `remove_dir_all`ed it at start and end. Two borg test processes running at once (two worktrees' `otto ci`, or `otto ci` beside a test loop, sharing one `TMPDIR`) delete each other's video and frames mid-run. The 0/300 sequential loop above never hit it; two copies of the binary run concurrently, 40 rounds: 37/80 runs failed, 32 at `tests.rs:333` (`extract_frames` returned Err), e.g. `ffmpeg frame extraction failed: [image2 @ ...] Could not open file : /tmp/claude-1000/borg-test-frames-synth/frames/frame_0001.jpg ... Input/output error` and `Failed to read frames dir: .../borg-test-frames-synth/frames: No such file or directory`; the rest at the frame-name/sidecar asserts (`:350`, `:351`, `:356`, `:358`). Same panic site as the investigation's 2/600.
- `test_extract_frames_disabled_returns_empty` had the same fixed-path shape (`borg-test-frames-disabled`); it never writes, but it is the same class.

### Fix
- Both tests use a `tempfile::tempdir()` per run (`borg/src/youtube/tests.rs`); the manual `remove_dir_all` calls are gone (the `TempDir` drop cleans up). `extract_frames` is unchanged.

### Break-it
- Old test, two concurrent processes x 40 rounds: 37/80 failed (above). New tests, same harness: 80/80 passed, 0 failed.
- `otto ci < /dev/null`: exit 0, "All CI checks passed!", 3056 passed / 0 failed.

## Orchestrator fix: closed-port test class

- Cause: a test needing "a port nothing listens on" bound `127.0.0.1:0`, read the port, and dropped the listener, then dialed it expecting connection refused. The port is free again, so a parallel test's stub (`bind("127.0.0.1:0")`) can be handed it by the kernel and answer. Observed once in 300 loop runs: `borg queue::tests::fetch_stub::fetch_connect_refused_names_the_address` got `PredatesQueue { addr: "127.0.0.1:46653" }`. Same class as the earlier `wait_closed_port_exits_one` fix.
- Sites (rg over `127.0.0.1:0`, `port: 1NNNN`, refused/not-listening wording): `borg/src/queue/tests.rs` (bind-drop), `oracle/src/queue/tests.rs` (bind-drop), `borg/src/replay/tests.rs` (bind, read port, `drop(reserved)`), `borg/src/client/tests.rs` (hardcoded `port: 19999`), `vault/src/daemon/client/tests.rs` (hardcoded port 1, dialed, expects `is_connect()`), `sb/tests/wait.rs` (inline socket2 hold). All six migrated. `wait_bogus_flag_is_clap_exit_two` passes port 1 but never dials: left alone.
- Helper: `vault::testnet::closed_port() -> ClosedPort` (`vault/src/testnet.rs`, test in `vault/src/testnet/tests.rs`), behind `test-util`. It owns a bound, never-listening `socket2::Socket`; `.port()` reads the port; dropping releases it. `socket2 = 0.6.3` is an optional vault dependency enabled by `test-util = ["dep:socket2"]`; sb's direct socket2 dev-dependency is removed. `Cargo.lock` diff is only the edge moving from sb to vault. Unit tests: connect is `ConnectionRefused`; a second bind fails `AddrInUse` while held and succeeds after drop; two helpers get distinct ports. `vault/AGENTS.md` module map updated (the `agents-map` task failed until it was).
- Guard: `bin/source-lint` check 5, test files only (same globs as check 1), three patterns each pointing at `vault::testnet::closed_port()`: a block binding `127.0.0.1:0` whose tail is `.port()` (bind-and-drop), a `bind("127.0.0.1:0")` followed by `drop(` before any `}`, and `port: 1NNNN`. Proof: a planted `borg/src/zzplant/tests.rs` carrying all three shapes -> exit 1, every violation printed with `borg/src/zzplant/tests.rs:<line>`; file removed -> "source-lint: clean", exit 0. The migrated tree is clean (exit 0).
- Break-it (deterministic, python probe under `~/.cache/scratch`): OLD idiom (bind, read port, close) then a stub binding that exact port and listening -> a dial of the "closed" port CONNECTED, so the old test logic sees an answer instead of refusal. NEW idiom (held bound socket) -> dial is refused and a stub bind fails "Address already in use". The old pattern collides whenever the kernel reuses the freed port, which is what the 1/300 observation was.
- `otto ci < /dev/null`: exit 0, "All CI checks passed!", 3059 passed. Separate commit, not part of any numbered phase.

## Phase 28: strip narration, borg
### Design decisions
- Comments and doc comments only, 81 files under `borg/` (src + tests): 347 lines removed, 340 added (`git diff --stat`). Probe `rg -c 'Phase [0-9A-Z]' --type rust borg` prints nothing, exit 1.
- Rubric applied: `Phase N` tag deleted and the sentence kept when it says why; where the tag was the only content it named (e.g. "Phase 5 publishes") the sentence now names the module (`publish`, planning, the pipeline handler) instead. Bare date attributions dropped ("2026-07-07 distillation-output-restore" -> "distillation-output-restore", "(2026-07-24)", "(2026-08-15 note-identity design)", "(per 2026-05-20 architect consensus)", "(2026-10-05 quality-review-fixes)"). Hyphen variants of the same tag also stripped: `Phase-N`, `post-Phase-6 cutover`, `pre-Phase-7`, `Phases 3-4`, `Phase 9c-*`, `Phase 7b` (~33 lines; the doc's probe regex does not match them but they are the same archaeology).
- First pass was a regex over comment lines only (parenthetical `(Phase N)`, leading `Phase N: `, `, Phase N)`), the remaining ~180 lines by hand; then a join of the orphan short lines the removal left in hard-wrapped comments.
- Proof no code changed: `~/.cache/p28/verify.py` strips comments (line, doc, nested block) while respecting string/raw-string/char literals, whitespace-normalises, and compares HEAD against the working tree for every touched file. 81 files checked, 2 differ, both are deliberate string edits: `borg/src/replay.rs` (the doc-named user-facing message) and one test assertion message in `borg/src/thread/tests.rs`. Every other file has an identical non-comment token stream. `otto ci` also green.
- `replay.rs` user-facing string: "staged before Phase 7" -> "staged before replay metadata was recorded" (the condition it means: the trace has no `members.yml`, written only by the harvest publish path).

### Kept dates (comment lines; each with its rubric reason)
Scar tissue / external observation:
- `borg/src/youtube.rs:331` 2026-09-28, 2026-10-02: the two 403s observed, cleared on manual retry (external observation)
- `borg/src/jina.rs:12` 2026-04-19: when Jina was seen blocking XDA/HowToGeek (external observation)
- `borg/src/stages/fetcher.rs:22` 2026-04-19: XDA Developers blocking bot IPs (external observation)
- `borg/src/migrate.rs:351` 2026-04-19: the audit that identified the 28 XDA notes (what broke)
- `borg/src/notify.rs:30` 2026-05-21: measured Telegram round-trip times on desk (observation behind the timeout)
- `borg/src/notify.rs:47` 2026-05-24: the incident the sentence describes (scar tissue)
- `borg/src/notify/tests.rs:273` 2026-05-24: regression for the `test_ingest_connection_refused` incident
- `borg/src/pipeline/tests.rs:497` 2026-07-04: vault-name-mismatch bug the guard pins
- `borg/src/pipeline/publish.rs:9` 2026-07-04: confirmed on Android, the failure mode being guarded
- `borg/src/pipeline/handlers.rs:246` 2026-09-02..09-22: window in which the log only showed the ERROR line
- `borg/src/pipeline/inflight.rs:6`, `borg/src/pipeline/atomic.rs:3`, `borg/src/pipeline/atomic/tests.rs:371` 2026-05-08: the incident the in-flight guard and atomic write fix
- `borg/src/harvest/contract.rs:25` 2026-08-15 reviewed, 2026-08-01 clyde bump: the contract-version bump the note records (version evidence)
- `borg/src/stages/classify.rs:3` 2026-03-30: identifies the superseded doc whose signatures the module inherits
- `borg/src/pipeline/session.rs:517` 2026-07-17: names the harvest doc whose "scope-tagged" wording this amends (identifies the doc, not an attribution)
Data identity / worked examples (the date IS the value):
- `borg/src/config/harvest.rs:40`, `borg/src/config/tests.rs:869` 2026-07-02: the real catalog slice the `min-msgs` tuning was measured against
- `borg/src/harvest/publish/tests.rs:111` 2026-07-02: golden-fixture session ids/timestamps being reused
- `borg/src/backfill/tests.rs:256,290`, `borg/src/retention/tests.rs:150` 2026-06-05/06-20 + 60d: arithmetic of the worked example the assertion checks
- `borg/src/retention.rs:28`, `borg/src/backfill.rs:6,124`, `borg/src/github.rs:40`, `borg/src/receipts.rs:367,369` : ISO-8601 / date format examples in doc text, not history
- `borg/src/types.rs:57`: date is part of a design-doc filename (see cites)
Also kept, not comments: code identifiers/strings/test data carrying dates, e.g. `produced_at: "2026-07-07..."`, and two assertion messages `pipeline/tests.rs:906,1006` ("(2026-07-07 policy)"), left because the phase is comments-only.

### Kept `docs/design/` cites (49 comment lines; each points at the reason for an invariant or the design that owns the behavior)
- Harvest note identity / trace-keyed replace (`2026-08-15-harvest-note-identity-trace-keyed-replace.md`): `tests/replay_lands_same_note.rs:2`, `tests/body_hash_agrees_across_paths.rs:2` (the guards are "required by" it), `harvest.rs:24`, `dedupe.rs:3`, `markdown.rs:104`, `receipts.rs:520`, `harvest/watermark.rs:40`, `markdown/tests.rs:744` (quotes the requirement), `pipeline/session.rs:67`, `harvest/identity.rs:2`
- Harvest clyde sessions (`2026-07-17-harvest-clyde-sessions.md`): `harvest.rs:5`, `config/harvest.rs:89`, `config/tests.rs:860`
- Harvest completion (`2026-07-20-harvest-completion.md`): `harvest/contract/tests.rs:153` (the RED->GREEN contract the test pins)
- Harvest watchdog (`2026-07-24-harvest-watchdog-cross-process-reaping.md`): `pipeline/permits.rs:35` (why permits are cross-process)
- Clyde export contract (`clyde/docs/design/2026-07-17-session-export-contract.md`, cross-repo prefix): `harvest/contract.rs:3`
- Discovery remediation (`2026-09-05-discovery-remediation.md`): `tests/empty_slug_publish_falls_back_to_trace_id.rs:2` (regression guard source)
- Shakedown v0.8.5 cleanup (`2026-05-20-shakedown-v0.8.5-cleanup.md`): `vault/src/rss.rs:10`, `vault/tests/candle-bounded.rs:4` (added by the implementation audit: missing from this inventory)
- Content-aware slide filtering (`2026-06-28-content-aware-slide-filtering.md`): `config.rs:606,677,709`, `slides.rs:75`, `slides/classify.rs:14`, `config/tests.rs:555`
- Frame-aware YouTube ingestion (`2026-04-29-frame-aware-youtube-ingestion.md`): `config.rs:747`, `slides.rs:7`, `slides/publish.rs:3`
- Video distill token budget (`2026-08-30-video-distill-token-budget.md`): `config.rs:1000` (why the cap exists)
- Signal transport (`2026-05-24-signal-as-borg-transport.md`, `2026-05-24-signal-state-dir-internalization.md`, `2026-05-28-signal-cold-start-bootstrap.md`): `signal.rs:6,730,750,884`, `signal/bootstrap.rs:15`, `signal/tests.rs:2` (privacy-load-bearing invariants)
- Receipts-log legacy markdown excision (`2026-06-03-receipts-log-legacy-markdown-excision.md`): `pipeline.rs:385`, `intake.rs:10`, `triage.rs:8`
- Thread title generation (`2026-07-08-thread-title-generation.md`): `pipeline.rs:835`, `thread.rs:8,102`
- Desktop notifications (`2026-05-21-desktop-notifications.md`, `2026-06-10-desktop-notification-replace-timeout.md`): `notify.rs:29,306` (why one shared 500 ms timeout)
- GitHub repos from video description (`2026-06-08-github-repos-from-video-description.md`): `github.rs:87` (denylist rationale)
- YouTube metadata pipeline redesign (`2026-03-22-youtube-metadata-pipeline-redesign.md`): `fabric.rs:43`, `pipeline/handlers.rs:75`
- Content-hash dedup (`2026-06-19-content-hash-dedup.md`): `audit.rs:31,373,791` (why the finding only REPORTS)
- Fabric pattern resolve and distill DLQ (`2026-05-18-fabric-pattern-resolve-and-distill-dlq.md`): `config/tests.rs:480` (why fetch timeouts are split)
- Ingest queue status (`2026-10-04-ingest-queue-status.md`): `queue.rs:6`
- Every unprefixed cite resolves to an existing `docs/design/` file (checked by script). `borg/src/extension/install.rs:26` cites `docs/postmortems/2026-06-07-...` inside a string literal, not a comment, untouched.
- Cites rewritten (the design name stays, tag/date dropped): `config.rs:709` (", Phase 1" dropped), `replay.rs:410,458` (the "design doc Phase N" prefix reduced to the quoted behavior), `pipeline/session.rs` several, `pipeline.rs:835`, `harvest/contract/tests.rs:153`.

### Deviations
- `borg/src/thread/tests.rs:339` assertion message "the Phase 2 override must prevent..." changed to "the title override must prevent...": a string, not a comment, but it is a `Phase [0-9A-Z]` hit and the success criterion requires the probe empty. Test behavior unchanged (message only).
- Not touched, left for a decision (see open questions): `borg/src/pipeline/session.rs:310` log string "(pre-Phase-2 watermark row)"; bare `P3`/`P6` design-section labels in comments (`pipeline/tags/tests.rs`, `pipeline/session/tests.rs`, `pipeline.rs`, `harvest.rs:109`, `pipeline/atomic/tests.rs:224`, `pipeline/tests.rs:780`); identifiers `phase7_distilled`/`phase7_slide_body` (`pipeline/tests.rs`) and `PHASE0_BULK`/`PHASE0_BODY`/`parses_phase0_bulk_envelope` (`harvest/contract/tests.rs`).
- Some rewrites rewrote a "Phase N" to a generic referent ("the spike", "the downstream validator", "a resolved design decision") where the surviving fact is not recoverable from the code alone; flagged only because a reviewer might prefer deleting the clause.

### Tradeoffs
- Strip hyphen variants too (`post-Phase-6 cutover`, `Phase-7`) vs only the `Phase [0-9A-Z]` probe: same archaeology and Phase 31's guard would otherwise be trivially bypassed by hyphenating; ~33 more comment lines in the same diff.
- Keep all unprefixed `docs/design/` cites vs prune "See X" cites with no stated invariant: rubric says keep when pointing at the reason; pruning needs per-cite judgment about whether the design is still the owner, and Phase 31 will mechanically verify they resolve.
- Join orphaned short comment lines (tool-assisted) vs leave ragged: the join only touches comment lines (re-verified token stream equal); leaving them would leave 1-5 word lines.

### Open questions
- Strip the bare `P3`/`P6` design-section labels, the `phase7_*`/`PHASE0_*` identifiers, and the `pre-Phase-2` log string in a later pass? They are not matched by the doc's probe or by Phase 31's pattern (`\bPhase [0-9A-Z]`); renaming identifiers or changing a log string is a code change outside this comments-only phase.

## Phase 29: strip narration, cortex
### Design decisions
- Comments and doc comments only, 42 files under `cortex/` (src + tests): 278 insertions, 282 deletions (`git diff --stat`). Probe `rg -c 'Phase [0-9A-Z]' --type rust cortex` prints nothing, exit 1 (was 218 lines in 39 files at start). Hyphen and split variants also stripped: `Phase-7`, `pre-Phase-N`, `Phases 1-4`, `Phase 3-5`, `Phase 0/7`, `Phase A5 / B2`, `Phase 7a`, `Phase 2a`, and one `Phase` / `10` wrap across a line break (`hub.rs:76`, invisible to the single-line probe). Also reworded the unnumbered "this phase / later phase / the graph phase" references that meant implementation phases (`config/tests.rs`, `sweep/tests.rs`, `opts.rs`, `migrate.rs`, `unlink.rs`, `graph/tests.rs`, `hub/tests.rs`, `linking/tests.rs`, `schema_docs/tests.rs`, `proposals.rs`). Structural uses of "read phase / inference phase / write phase" in `embed.rs` and `embed/tests.rs` describe the loop's own stages, not archaeology, and stay.
- Rubric as Phase 28: the tag is deleted and the sentence kept when it says why; where the tag was the only content, the sentence now names the thing (e.g. "pre-Phase-8 code" -> "the old code", "Phase 4 owns it" -> "forward bridging owns it", "Before this phase" -> "Previously", "Phase 3 of graph-augmented-memory" -> "graph-augmented-memory"). Section dividers lost the tag and got a descriptive label (e.g. `// -- Phase 3: merge executor` -> `// -- merge executor`; `entity-hub-two-vector-synthesis Phase 1` in `config/tests.rs`, `graph/tests.rs`, `hub/tests.rs`, `embed/tests.rs` -> a label for what the section actually tests, read from the tests beneath). The `association.rs` module header, which narrated Phases 1-5 as a changelog, now lists the layers.
- `daemon/tests.rs` long comment block for `periodic_sweep_fingerprint_converges_after_phase2` and the full-action-set test was rewritten to state the invariant and the bite evidence (commit `803255e`) without the phase numbering.
- Method: first a regex pass over comment-only lines (parenthetical `(Phase N)`, `, Phase N)`, `doc`, Phase N`, leading `Phase N: `, `pre-Phase-N `), 80 lines; the remaining ~140 by hand with exact-string replacements that assert a single match. Bare dates dropped where they were attributions ("2026-07-24 cortex-association-sweep design", "2026-07-05 retrieval-gate remediation", "(Scott, 2026-07-20)", "(panel round 1, Mode 2, 2026-09-21)", "(2026-10-05 quality-review-fixes)", "2026-08-18 governance-starvation fix", etc., about 20 sites).
- Proof no code changed: `~/.cache/p29/verify.py` (rewritten; the Phase 28 script was gone) strips line/doc/nested-block comments while respecting string, raw-string, and char literals, whitespace-normalises, and compares `git show HEAD:<file>` against the working tree for every touched file. 42 files checked, 3 differ, all deliberate assertion-message edits (see Deviations). For those 3, applying the same replacement to the HEAD text and re-comparing gives an identical stream (True for all three). `cargo fmt --all --check` clean. `otto ci < /dev/null`: exit 0, "All CI checks passed!", 3059 passed, 8 ignored (summed from the `test result:` lines), no failures, no retries.

### Kept dates (comment lines; each with its rubric reason)
Scar tissue / external observation:
- `cortex/src/embed.rs:75,95` 2026-08-16: the tick that ran two days on an AVX-only host and starved classify (what broke); `config.rs:283` same incident
- `cortex/src/embed.rs:794` 2026-05-19: when the memory blowup was observed on the daemon
- `cortex/src/embed/tests.rs:584` 2026-05-19: the OOM the regression guard pins
- `cortex/src/config.rs:328` 2026-07-05 with nDCG 0.8795 -> 0.8471: the eval-gate failure and its numbers (measurement)
- `cortex/src/config.rs:773` 2026-07-24: when the 213 live rows were probed (external observation)
- `cortex/src/scope.rs:160`, `scope/tests.rs:111` 2026-05-19: the audit that found 342 notes silently dropped (what broke)
- `cortex/src/opts.rs:102`, `unlink/tests.rs:154,155` 2026-06-11/06-12 with commit `fa3f9a8` and obsidian `017363e3`: ordering of the exemption commit vs the hub mint, the cause of the false links (SHA evidence)
Data identity / worked examples (the date IS the value):
- `cortex/src/hub.rs:488` 2026-07-02: the Fabric pass a provenance stamp on hub bodies names
- `cortex/src/hub/render/tests.rs:159` 2026-08-01: the date group the assertion checks
- `cortex/src/intel.rs:238` 2026-05-18: example path `notes/ai/daily/2026-05-18`
- `cortex/src/schema_docs/tests.rs:7` 2026-09-05: the fixed timestamp value
Design-doc ids that embed their date (the date is part of the name, see cites): `cortex/src/config.rs:29,30` (`2026-07-07-distillation-output-restore`, `2026-09-21-staged-tag-proposals`), `embed.rs:737,1135` (`2026-07-07-distillation-output-restore`).
Also kept, not comments: fixture/test data and strings carrying dates (`date: 2026-03-18`, `"---\ntitle: ..."` frontmatter in `testutil.rs`, `classify/tests.rs`, `summarize/tests.rs`, `frontmatter/tests.rs` file names `daily/2026-03-18.md`, `naming/tests.rs:20`, `hub/render/tests.rs:44`).

### Kept `docs/design/` cites (39 comment lines across 28 files; each points at the reason for an invariant or the design that owns the behavior)
All unprefixed and all resolve to an existing `docs/design/` file (checked by script; the two script "MISS" rows, `sessions/2026-08-01-oracle.md` at `hub/render/tests.rs:44` and `2026-03-16-daily.md` at `naming/tests.rs:20`, are fixture note names, not cites).
- Cortex daemon oscillation loop (`2026-07-05-cortex-daemon-oscillation-loop.md`, 14 cites): `daemon.rs:521`, `daemon/tests.rs:184,369,521,848,923,985`, `tests.rs:130`, `intel/tests.rs:41`, `classify.rs:43`, `lib.rs:100,265`, `sweep/tests.rs:441`, `embed/tests.rs:391` (the success criteria the tests pin and the shared-scan seam)
- Harvest note identity / trace-keyed replace (`2026-08-15-harvest-note-identity-trace-keyed-replace.md`): `association.rs:39`, `association/tests/session_identity.rs:4`, `classify.rs:898`, `classify/tests.rs:135` (grouping and numeric-collision invariants)
- Staged tag proposals (`2026-09-21-staged-tag-proposals.md`): `config/tests.rs:267`, `sweep/tests.rs:412,593,689` (why `staging-root` is one top-level key, null-reject and write semantics)
- Entity hub two-vector synthesis (`2026-08-15-entity-hub-two-vector-synthesis.md`): `config.rs:108`, `hub/render.rs:2`, `hub/asymmetry.rs:2`, `tests/hub_retrieval_contract.rs:2`
- Cortex embed memory bounding (`2026-05-19-cortex-embed-memory-bounding.md`): `config.rs:273`, `embed.rs:67` (why the chunk cap exists)
- Cortex association sweep (`2026-07-24-cortex-association-sweep.md`): `association.rs:22`
- Hybrid retrieval (`2026-05-16-hybrid-retrieval-fts5-vector-rrf.md`): `embed.rs:4`; graph-augmented memory (`2026-06-05-graph-augmented-memory.md`): `graph.rs:5`
- Fabric pattern resolve and distill DLQ (`2026-05-18-fabric-pattern-resolve-and-distill-dlq.md`, wrapped across two lines): `summarize.rs:310`
- Named without a path (still resolve): `2026-07-07-distillation-output-restore` at `config.rs:29`, `embed.rs:737,1135`, `embed/tests.rs:140`, `summarize.rs:340`; `2026-09-21-staged-tag-proposals` at `config.rs:30`; `harvest-completion`, `harvest-clyde-sessions`, `cortex-association-sweep`, `entity-hub-two-vector-synthesis`, `graph-augmented-memory` in prose.

### Non-comment leftovers for Phase 31 to rename (not touched here)
- Identifier `periodic_sweep_fingerprint_converges_after_phase2` (`cortex/src/daemon/tests.rs:479`; also named in the doc comment at `:527`, rename both together)
- Identifier `full_action_set_periodic_sweep_fingerprint_converges_after_all_phases` (`cortex/src/daemon/tests.rs:573`; named in the doc comment at `:477`)
- No log strings and no `PHASE0`-style constants with "phase" in cortex outside those two.
- Bare `P<N>` design-section labels, all in comments, not matched by the probe or by Phase 31's pattern: `migrate/tests.rs:548,564` (P4), `frontmatter/tests.rs:80,263` (P9, P11), `classify/tests.rs:198` and `classify.rs:778` (P3 union), `migrate.rs:558,559,709` (P4, P9), `tags/tests.rs:6,76,156,209,210` (P9, P4, P1), `sweep/tests.rs:539` (P4), `frontmatter.rs:72` (P9), `tags.rs:12,253` (P9, P4), `schema_docs/tests.rs:198` and `schema_docs.rs:2,234` (P9, P8), `lib.rs:188` (P9).

### Deviations
- Three test assertion messages (strings, not comments) changed because they are `Phase [0-9A-Z]` hits and the success criterion requires an empty probe; test behavior unchanged: `cortex/src/daemon/tests.rs:516` ("...EMPTY fingerprint after Phase 2" -> "...EMPTY fingerprint"), `cortex/src/embed/tests.rs:673` ("pre-Phase-9 title+summary" -> "pre-capture-note title+summary"), `cortex/src/sweep/tests.rs:675` ("no staged arm until Phase 6" -> "no staged arm was written"). Verified token-stream equal to HEAD modulo exactly these three.
- The design doc counts 222 lines; the tree had 218 on this branch.
- Several rewrites replace a "Phase N" referent with a generic one ("the matcher reconciliation", "the lint-fingerprint fix", "the shared-scan seam", "a consolidation that replaced two weaker copies", "recommended in review") where the numbered phase was the only name for it and the surviving fact is not otherwise recoverable from the code; flagged because a reviewer might prefer deleting the clause.

### Tradeoffs
- Rewrite long phase-narrating test doc blocks (`daemon/tests.rs`, `association.rs` header) to the invariant vs delete outright: the blocks carry the bite evidence (`803255e`) and the invariant, which is scar tissue under the rubric.
- Drop dates on design-attribution parentheticals vs keep: the design ids are already cited by filename or by name, so the date adds no evidence; kept where a date marks an incident, observation, SHA, or is the data.
- Reword unnumbered "this phase" references vs leave: they are the same archaeology without the digit and would survive Phase 31's pattern; each is a one-clause edit.

### Open questions
- None new. The P<N> label and identifier questions from Phase 28 now also cover the cortex items listed above.

## Phase 30: strip narration, the rest
### Design decisions
- Comments and doc comments only: 78 source files (`vault/` 35 files, `distillers/` 25, `oracle/` 7, `sb/` 7, `bin/` 4 Rust/Python) plus 3 living docs (`CLAUDE.md`, `distillers/AGENTS.md`, `cortex/AGENTS.md`); 81 files, 334 insertions, 338 deletions. Probe 1 `rg -c 'Phase [0-9A-Z]' --type rust borg cortex vault distillers oracle sb bin` prints nothing (exit 1). Probe 2 `rg -n '\bPhase [0-9A-Z]' CLAUDE.md -g 'AGENTS.md' .` prints nothing (exit 1). Hyphen and split variants also cleared in scope (`Phase-N`, `pre-Phase-N`, `Phase 9c-hotfix`, `Phase A3`/`B2`/`7b`, `Phase 8/9`, a `Phase`/`1` wrap in `vault/src/distilled.rs`, "this phase" references that meant implementation phases). `oracle/src/eval.rs` "Phase A/B/C" were the eval's own stages but read as plan archaeology; rewritten to describe what each step does.
- Method: a regex pass over comment-only lines (parenthetical `(Phase N)`, `(<design>, Phase N)`, `, Phase N)`, leading `Phase N: `, `pre-Phase-N <noun>` -> `older`; 104 lines), then ~190 hand edits as exact-string replacements asserting a single match each; then a pass over the remaining dated comments. Two of the regex results were wrong (the leading-`Phase N:` rule had stripped mid-sentence continuation lines at `vault/src/search/schema.rs` `ensure_repo_columns` / `ensure_repos_touched_columns` doc comments and `distillers/src/tags.rs` `merge_shard_scores`); found by a script that prints the previous line of every such edit, repaired by hand.
- Rubric as Phases 28-29: tag deleted and sentence kept where it says why; where the tag was the only name the sentence now names the thing ("Phase 6 dispatcher" -> "The dispatcher", "pre-Phase-3 shape" -> "legacy shape"). Bare design-attribution dates dropped ("2026-07-07 distillation-output-restore" -> "distillation-output-restore", "Resolved Decision 2026-07-07" -> "distillation-output-restore Resolved Decision", "harvest distill-parsing robustness, 2026-07-24", "harvest-content-slug-naming, 2026-07-24", "as of the 2026-07-05 distillation overhaul", "(design doc Addendum D, 2026-10-05)", "as of the 2026-06-09 remediation").
- Proof no code changed: `~/.cache/p30/verify.py` strips line/doc/nested-block comments respecting string, raw-string and char literals (Python files: AST with docstrings dropped), whitespace-normalises, and compares `git show HEAD:<file>` against the working tree for every touched source file. 78 files checked, 1 differs: `vault/src/search/schema.rs`, whose only non-comment change is the SQL `--` comment line inside the `ensure_vec_schema` raw string (the doc-named `:158` site, "-- Phase 3 (docs/...):" -> "-- See docs/...:"). Applying that one replacement to the HEAD text and re-comparing gives an identical stream (True). `bin/gen-bge-reference.py` edit is its module docstring only. `cargo fmt --all --check` clean. `otto ci < /dev/null`: exit 0, "All CI checks passed!", 3059 passed, 0 failed, 8 ignored (summed from the `test result:` lines), run twice (before and after the date pass) with the same counts, no retries.
- Living docs: `CLAUDE.md:54` "(2026-09-21-staged-tag-proposals Phase 3; an always-write ...)" -> "(2026-09-21-staged-tag-proposals; an always-write ...)"; `distillers/AGENTS.md:24,31,48` "as of Phase B2" / "(Phase B2)" removed, the regression-guard fact kept; `cortex/AGENTS.md` (line 59 on this tree, not 58) "Phase 5 MemGraphRAG" -> "MemGraphRAG". The structural uses of "three-phase loop" and "only phase that retracts" in `cortex/AGENTS.md:26,34` describe the code and stay.

### Kept dates (comment lines; each with its rubric reason)
Scar tissue / external observation:
- `vault/src/paths/tests.rs:174`, `vault/src/paths.rs:422`, `sb/src/cli/bootstrap.rs:364` 2026-08-15: the `sb bootstrap` run that baked its CWD into the unit's `--vault` and let cortex rewrite 203 files (what broke)
- `distillers/src/validate.rs:216`, `distillers/src/validate/tests.rs:209` 2026-05-18: the cortex backfill that lost data when article/repo distillers left a field `None` (what broke)
- `distillers/src/tags.rs:26` 2026-09-20: when the tool schema's 100-label ceiling was observed (external observation)
Data identity / design ids that embed their date (the date is part of the name; see cites):
- `sb/src/cli/oracle.rs:54` (design 2026-06-06), `sb/src/cli/borg/args.rs:142,145,149` (design 2026-07-05, 2026-07-17, 2026-08-15), `sb/src/cli/bootstrap.rs:393` (design doc 2026-07-20 harvest-completion), `vault/src/trace/tests.rs:11` and `vault/src/tombstone.rs:6` (2026-08-15 note-identity filenames), `vault/src/paths.rs:307,308` (`2026-07-07-distillation-output-restore`, `2026-09-21-staged-tag-proposals`), `vault/src/distilled.rs:67` (`2026-09-21-staged-tag-proposals.md`), `sb/src/cli/bootstrap/tests.rs:167` (`2026-09-21-staged-tag-proposals.md`), `bin/strip-transcripts/src/main.rs:10` 2026-06-28 (the cutoff the sweep implements, also `CUTOFF_RFC3339`)
- Format examples, not history: `vault/src/frontmatter.rs:19`, `vault/src/distilled.rs:365`, `distillers/src/repo.rs:37`, `distillers/src/video.rs:54`, `sb/src/cli/borg/args.rs:324,325` (ISO-8601 / date examples)
Also kept, not comments: fixture rows and SQL literals carrying dates (`vault/src/table/tests.rs`, `vault/src/ledger/tests.rs`, `vault/src/search/tests/group_a.rs`, `vault/src/search/tests/filters.rs`).

### Kept `docs/design/` cites (29 path occurrences in scope; each points at the reason for an invariant or the design that owns the behavior)
All unprefixed and all resolve to an existing `docs/design/` file (checked by script over every `docs/design/<name>.md` in scope; no MISS).
- Hybrid retrieval FTS5+vector+RRF (`2026-05-16-hybrid-retrieval-fts5-vector-rrf.md`): `vault/benches/hybrid.rs:17`, `vault/src/embedding.rs:4`, `vault/src/search/vector.rs:5` (the vector scan design)
- Candle embedding backend (`2026-05-17-candle-embedding-backend.md`): `vault/src/embedding.rs:6`, `embedding/candle.rs:5`, `embedding/fastembed.rs:3` (why the fastembed adapter is relocated), `vault/tests/regression/candle/parity.rs:3` (the parity bar)
- Cortex embed memory bounding (`2026-05-19-cortex-embed-memory-bounding.md`): `vault/src/rss.rs:9`
- Signal (`2026-05-24-signal-state-dir-internalization.md`, `2026-05-28-signal-cold-start-bootstrap.md`): `sb/src/cli/checks.rs:1385`, `vault/src/paths.rs:285` (privacy-load-bearing)
- Receipts-log legacy markdown excision (`2026-06-03-...`): `vault/src/intake.rs:9`
- Graph-augmented memory (`2026-06-05-graph-augmented-memory.md`): `vault/src/search/graph.rs:5`
- Configurable retrieval pipeline (`2026-06-06-configurable-retrieval-pipeline.md`): `oracle/src/config.rs:37`, `oracle/src/config/tests.rs:4`, `vault/src/search/rerank.rs:2`
- Oracle eval relevance lift (`2026-06-06-oracle-eval-relevance-lift.md`): `oracle/src/eval.rs:5`
- Cortex daemon oscillation loop (`2026-07-05-cortex-daemon-oscillation-loop.md`, and its `-implementation-notes.md` at `vault/src/logging.rs:14`, the measured info-level log rate): `vault/src/search/schema.rs:158`, `schema/tests.rs:250`, `vector.rs:628` (the examined-sentinel invariant)
- Distillation knowledge extraction (`2026-07-05-distillation-knowledge-extraction.md`): `vault/src/search/schema.rs:196`, `vector.rs:45` (why the `claim` kind exists)
- Distillation output restore (`2026-07-07-distillation-output-restore.md`): `bin/strip-transcripts/src/lib.rs:4`, `main.rs:11` (the sweep's owner)
- Tags-only classification (`2026-09-19-tags-only-classification.md`): `vault/src/ledger.rs:120`
- Ingest queue status (`2026-10-04-ingest-queue-status.md`): `sb/src/cli/borg/wait.rs:3`, `sb/tests/wait.rs:4`, `vault/src/queue.rs:6`
- This doc (`2026-10-05-quality-review-fixes.md`, Addendum D): `vault/src/search/vector.rs:223` (measured p50 behind the perf ceiling). Note: this doc is still uncommitted, so the cite resolves in the working tree only until the doc lands.
- Cites reworded (design name stays, phase/date tag dropped): `vault/src/search/schema.rs:158` ("Phase 3 (path):" -> "See path:"), `schema.rs:196` and `vector.rs:45` ("Phase 9 of path" -> "see path"), `bin/strip-transcripts/{main,lib}.rs` ("Phase 6 of path" -> "See path"), many bare design names in prose ("(harvest-completion Phase 4)" -> "(harvest-completion)").
- Named without a path: `distillation-output-restore Resolved Decision` at `vault/src/distilled.rs:42,391`, `distillers/src/{article:117,video:139,validate:166}.rs`.

### Non-comment leftovers for Phase 31 to rename (not touched here)
- Identifiers: `pattern_yaml_without_new_phase4_keys_still_parses` and `reduce_yaml_without_new_phase4_keys_still_parses` (`distillers/src/parse/tests.rs:399,414`)
- `eprintln!` strings: `"PHASE6-MEASURE article reduce-input: ..."` (`distillers/src/article/tests.rs:598`) and `"PHASE6-MEASURE thread reduce-input: ..."` (`distillers/src/thread/tests.rs:520`)
- Test fixture YAML in a raw string: `summary: "A pre-Phase-3 article."` and `summary: "A Phase-3 article with rich claims."` (`vault/src/distilled/tests.rs:296,309`)
- Data file: `distillers/src/tags/fixtures/classifier-dev-2026-09-20.json` lines 3, 7, 8, 10 ("Phase 0", "Phase 0b" in `source`/`note` strings; JSON has no comments, and it is a recorded measurement artifact)
- Cargo metadata: `bin/strip-transcripts/Cargo.toml:6` `description = "... (Phase 6, docs/design/2026-07-07-distillation-output-restore.md)"`
- Bare `P<N>` design-section labels in comments (not matched by either probe or Phase 31's pattern): `distillers/src/tags/tests.rs:214,366` (P1), `vault/src/schema.rs:17` (P3), `vault/src/search/tests/group_b.rs:1157` (P8), `vault/src/search/stats.rs:210` (P5), `oracle/src/server/tests.rs:858` (P8). Plus the Phase 28/29 lists in borg and cortex.
- The `distillers/src/parse/tests.rs` test names above are the only identifiers; no log strings with "phase" in scope.

### Deviations
- `bin/gen-bge-reference.py` module docstring edited (a Python docstring, the script's comment-equivalent): "the Phase 3 parity test" -> "the parity test". AST-equal modulo the docstring.
- `vault/src/search/schema.rs:158` is a SQL `--` comment inside a Rust raw string, so it is a non-comment token change for the Rust verifier. Named in the task as in scope; verified as the only difference.
- The task listed `cortex/AGENTS.md:58`; the hit is on line 59 on this tree. The design doc's `distillers/AGENTS.md` and `CLAUDE.md` line numbers matched.
- The design doc counts 745 lines across all crates (455 non-test, 290 test); scope for this phase had 297 lines matching `phase` case-insensitively before edits (`vault` 135, `distillers` 126, `oracle` 19, `sb` 10, `bin` 7), consistent with the doc's total once Phases 28-29 (borg 275, cortex 222 observed) are subtracted.
- No assertion messages needed changing in this scope (unlike Phases 28 and 29).
- Some rewrites replace a "Phase N" referent with a generic one ("the reduce step", "the embed loop", "the bridge backfill", "the shard-invariance trial", "the claim-kind work", "older/legacy shape") where the numbered phase was the only name; the surviving fact is not otherwise recoverable from the code. Flagged because a reviewer might prefer deleting the clause. Two invented-referent risks deserve a glance: `vault/src/search/index.rs:174` ("rendered ... by cortex", was "by Phase 8") and `vault/src/search/vector.rs:14` (the stale "Phase A reads only summary rows; Phase B3 will add max-pool" note, rewritten to a neutral "Summary rows are the base case; max-pool aggregation ... rides the same storage and API shapes", since B3 has since shipped).
- One stale claim fixed while there: `distillers/src/dispatcher.rs` Session doc said the arm "fails loudly until" the distiller is wired; it is wired, now "Routed to `SessionDistiller`."

### Tradeoffs
- Strip hyphen/split/unnumbered variants vs only the `Phase [0-9A-Z]` probe: as in Phases 28-29, the same archaeology would otherwise survive Phase 31's guard by hyphenating.
- Drop bare design-attribution dates vs keep: the design is already named, so the date adds no evidence; dates kept where they mark an incident, observation, or are part of the design id.
- Leave `P<N>`, `PHASE6-MEASURE`, `*_phase4_*` test names, the JSON fixture and Cargo.toml description vs edit now: all are code/data/metadata, outside a comments-only phase; listed for Phase 31.

### Open questions
- Same as Phases 28-29, now covering vault/distillers/oracle too: should Phase 31 (or a follow-up) rename `*_phase4_*` tests, the `PHASE6-MEASURE` strings, the fixture YAML strings, and strip the bare `P<N>` labels, and should the `classifier-dev-2026-09-20.json` fixture notes and `bin/strip-transcripts/Cargo.toml` description be reworded?
- The kept cite to `2026-10-05-quality-review-fixes.md` (`vault/src/search/vector.rs:223`) depends on this design doc being committed eventually; Phase 31's cite-resolution check will fail on a clean checkout until it is.

## Phase 31: guard the archaeology
### Design decisions
- Two checks added to `bin/source-lint` (numbered 6 and 7 in its header): (6) plan-phase tags, (7) design-doc cites resolve. Same `hits`/rg-exit-2 discipline and explicit rg paths as checks 1-5.
- Phase pattern widened beyond the doc's `\bPhase [0-9A-Z]` to `\bPhases? ?-?[0-9A-Z]`, so `Phase-N`, `pre-Phase-N`, `Phases N-M` fail too (the variants Phases 28-30 stripped; otherwise the guard is dodged by hyphenating). Scope: every `.rs` under the crate dirs (tests included), `AGENTS.md` under them, and `CLAUDE.md`. `docs/design/` is not linted (point-in-time).
- A second Phase check catches phase-numbered identifiers (`-i '\bphase_?[0-9]\w*|\w*_phase_?[0-9]\w*'`, `.rs` only), so the `phase7_*` / `PHASE0_*` / `*_phase2` class cannot return by naming.
- Cite check: `rg -o` lists `docs/design/<name>.md` tokens in `.rs`; a leading character in `[A-Za-z0-9_/.~-]` is excluded, so repo-prefixed cross-repo cites (`other-repo/docs/design/x.md`) and `~/` paths are skipped. Each remaining cite is tested with `-f` against the repo root.
- Carried from Phases 28-30 (Scott's direction "fix everything, no shortcuts"): the non-comment leftovers they listed were renamed or reworded in this commit, so the guard lands on a tree with zero exceptions. Names say what the test checks, not when it was written:
  - `cortex/src/daemon/tests.rs`: `periodic_sweep_fingerprint_converges_after_phase2` -> `intel_sweep_fingerprint_converges_without_digest_tag_fight`; `full_action_set_periodic_sweep_fingerprint_converges_after_all_phases` -> `full_action_set_periodic_sweep_fingerprint_converges`; both doc-comment mentions updated.
  - `distillers/src/parse/tests.rs`: `pattern_yaml_without_new_phase4_keys_still_parses` / `reduce_yaml_without_new_phase4_keys_still_parses` -> `*_without_enrichment_keys_still_parses`.
  - `borg/src/pipeline/tests.rs`: `phase7_distilled` -> `transcript_distilled`, `phase7_slide_body` -> `transcript_slide_body`; the two "(2026-07-07 policy)" assertion messages lose the bare date attribution (date rubric: the policy is named by the message, the date adds no evidence).
  - `borg/src/harvest/contract/tests.rs`: `PHASE0_BULK` / `PHASE0_BODY` -> `BULK_ENVELOPE` / `BODY_ENVELOPE`; `parses_phase0_bulk_envelope` -> `parses_bulk_envelope`.
  - `borg/src/pipeline/session.rs`: log string "(pre-Phase-2 watermark row)" -> "(watermark row predates trace recording)".
  - `distillers/src/article/tests.rs`, `thread/tests.rs`: `eprintln!` "PHASE6-MEASURE" -> "MEASURE".
  - `vault/src/distilled/tests.rs`: fixture summaries "A pre-Phase-3 article." / "A Phase-3 article with rich claims." -> "A legacy-shape article." / "An article with rich claims." (no test asserts the summary literal; verified by rg).
  - `distillers/src/tags/fixtures/classifier-dev-2026-09-20.json`: only the `source` and `note` fields reworded (Phase 0/0b/P1/OQ8 references); still valid JSON, no asserted data touched.
  - `bin/strip-transcripts/Cargo.toml` description: "(Phase 6, docs/design/...)" -> "(see docs/design/...)".
  - Bare `P<N>` design-section labels in comments (about 50 lines across borg, cortex, vault, distillers, oracle: every site in the Phase 28/29/30 lists) reworded to name the rule ("block-form migration", "reingest union", "tags-sibling", ...) or dropped. `rg '\bP[0-9]+[a-z]?\b'` over the crates, `.rs` only, now prints nothing.
- Design doc and this notes file are staged in this commit (they were untracked), so the cite at `vault/src/search/vector.rs:223` resolves on a clean checkout. The doc's `Status:` line is untouched for the orchestrator.

### Deviations
- Pattern is wider than the doc's literal `\bPhase [0-9A-Z]` (hyphen, `Phases N-M`, identifiers): same intent, closes the bypass Phases 28-30 flagged.
- Fix scope is wider than the doc's Phase 31 text (which lists only the two lint checks): the renames above were added by the orchestrator's direction, not by the doc.
- Renamed test names: `intel_sweep_fingerprint_converges_without_digest_tag_fight` names what the narrow fixture proves (the doc comment says convergence follows removal of the digest-tag fight); the other names drop only the phase suffix.

### Tradeoffs
- Case-insensitive identifier check (`phase_?N`) vs case-sensitive: the identifiers appeared as `PHASE0_`, `phase7_`, `_phase2`; case-insensitive catches all three at the cost of also catching a legitimate word like `phase1` in a future name, which is the point.
- Cite check parses `rg -o` output in bash vs a PCRE lookbehind: default rg has no lookbehind and `-P` may be missing; a leading-character class works on the default engine.
- Drop `P<N>` labels vs replace with section names: no design section numbers survive in the doc names; descriptive words age better.

### Open questions
- None.

### Proof (success criteria)
- `bin/source-lint` exits 0 on the final tree (PASS). `otto ci < /dev/null` exits 0, 3059 tests passed, the `[source-lint]` task ran (`source-lint: clean`).
- Planted `// Phase 3 wires this` in `borg/src/lib.rs`: exit 1, `borg/src/lib.rs:121: ... plan-phase tag` (PASS). Planted `// see docs/design/1999-01-01-x.md`: exit 1, `borg/src/lib.rs:121: cites docs/design/1999-01-01-x.md, which does not exist` (PASS). Planted `// pre-Phase-2 shape`: exit 1 (PASS). Also exit 1: `// Phases 3-4 did it`, `fn phase7_x() {}`, `Phase 4 note` in `CLAUDE.md`. A repo-prefixed `other-repo/docs/design/1999-01-01-x.md` is skipped (clean). All plants reverted; `git status` shows `borg/src/lib.rs` and `CLAUDE.md` unmodified.

## Orchestrator: implementation audit fold-in

Panel round 2 (run dir `/tmp/review-panel/NXcIoLGX/`, synthesis + `probes.md`); round 1 never launched (sandbox bridge down). 4 must-fix, 10 cheap-win, 5 defer. Every code fix carries a test that was run against the pre-fix code and failed, except the HOME lock (a race; no deterministic break).

- Branch rebased onto `main` at v0.15.14 (`4233f93`). `0166890` on main is the same options.js hostname fix as Phase 25; the conflict resolved to main's text, so the Phase 25 commit now carries only its test.
- Gate-0 rejections replay: `stage_0_init` stages the capture before the gate (`borg/src/stages/raw.rs`). Test `gate_0_rejection_is_recoverable_by_replay` (replay dry-run finds the trace and re-POSTs the original URL).
- WAL retry bounded: 5 s wall-clock deadline, `busy_timeout` zeroed for the loop (`borg/src/receipts.rs`). Break-it: without the zeroing the bound test took 5.0 s against a 1 s limit.
- `vault::process::run` detaches the drain threads on timeout instead of joining them. Break-it: a `setsid` descendant held the old code 10.0 s.
- source-lint: check 2 tolerates attributes and comments between `#[cfg(test)]` and `mod`; check 5 catches a listener temporary dropped in the statement that reads its port. Both verified on planted files.
- `try_exists` in `Blocklist::from_file` and `index_changed`: a stat error fails Gate-0 closed and keeps the index row.
- Gated `SearchIndex::insert_test_note_graph` (`test-util`) and `OracleMcpServer::with_borg_config` (`#[cfg(test)]`).
- Doctor cortex drift renders with the `--vault` read back from the installed ExecStart (`installed_vault_arg`).
- `search_vector` note_type/status filters tested both directions; `index_vault` stat-outside-transaction ordering tested via `is_autocommit`.
- oracle `HomeGuard::hold()` for tests that read `$HOME`-derived paths.
- `cortex::graph::record_watermark`: one helper for both passes, SQL errors propagate.
- Hotkey install prints the daemon target on every platform; the wrapped design-doc cite in `cortex/src/summarize.rs` unwrapped; `--rebuild` help and doc wording say what a post-swap failure leaves.
- Riding, disclosed: Phase 9 per-entry-point timeout matrix partial; checkpoint/manifest WARNs guarded by the rg criterion only.
- Carried, not fixed: ExecStart is `argv.join(" ")` (`vault/src/systemd.rs:236`), so a path with a space breaks the unit (pre-existing for exe/vault); source-lint check 7 still cannot see a cite wrapped across comment lines.

