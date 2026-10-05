# Design Document: Quality Review Fixes

**Author:** Scott Idler (drafted with Claude)
**Date:** 2026-10-05
**Status:** Implemented
**Review Passes Completed:** 5/5

## Summary

A side-by-side quality review against `tatari-tv/rerun` turned up 20 flaws in second-brain (`docs/handoff/quality-review-flaws.md`). Verifying them turned up more of the same kinds. This doc fixes all of them except F1 (borg's open write API), which stays as-is by Scott's call and gets an advisory addendum. Every fix carries a test or a guard that fails if someone reverts it.

## Problem Statement

### Background

- second-brain scored about 7.5/10 against rerun's 6.5. CI is strict (`clippy -D warnings`, a 1,500-line file cap, the full suite), `vault` is one shared type crate, and MCP schemas have a parity test.
- The flaws are what keeps it from an 8: silent failure paths, duplicated primitives that have already drifted, comments that lie, and tests that pass without testing.
- Four research passes re-checked every flaw against `01408d3`, swept each one's whole class, and found more instances than the handoff listed. Those extra instances are in scope under Scott's direction: "we are fixing everything. no shortcuts" (2026-10-05).

### Problem

Grouped by kind. `[H]` = in the handoff; `[R]` = found in research.

**Silent wrong answers and hangs**
- F2: six subprocess call sites can hang until timeout once output exceeds the 64 KiB pipe buffer. `[H]` 3, `[R]` 3.
- F3: 7 reqwest clients have no timeout; 3 fall back to `Client::new()` when the builder fails, silently dropping the configured timeout. `[H]`
- F4: error swallowing that loses state. The worst cases `[R]` read a YAML file, treat a parse error as empty, and write it back, destroying the file. `[H]` 4 sites, `[R]` 19 more.
- F14: oracle reads the tag vocabulary from the default path and fails open, so `tag_brief` calls canonical tags unknown. `[H]`
- F17: `knowledge_search` `limit` above 50 silently returns at most 50. `[H]`
- F21 `[R]`: the hotkey installer binds `sb ingest --clipboard`, a subcommand that does not exist, and the launchd plist runs `sb daemon --start`, which is broken the same way.
- F22 `[R]`: `reingest-failed` (`borg/src/migrate.rs:405`) sends no bearer token, unlike every other daemon client.
- F23 `[R]`: the extension options page builds its permission origin from `url.host` (includes the port), so Save refuses `http://desk.lan:8181` (`borg/clients/extension/options.js:19`). Already logged in dotfiles as Gap 10.

**Duplicated primitives that drifted**
- F5: 7 wikilink regexes in 3 variants plus 4 string parsers; inbound-link counting misses all 5,875 `[[dir/x]]` links in the live vault; dedupe can archive a note still linked as `[[note#heading]]`. `[H]` 6, `[R]` 1 regex + parsers.
- F6: one dynamic SQL filter builder copied 7 times. `[H]`
- F7: three systemd unit renderers; borg's has three accidental differences, and the installed `cortex.service` has drifted from its renderer. `[H]` 2, `[R]` 1 + drift.
- F8: two backoff implementations. `[H]`

**Lying docs and comments**
- F9: the workspace lint comment says `unwrap_used` cannot be workspace-level; it can (`allow-unwrap-in-tests`). cortex and oracle carry no deny. `[H]`
- F10: a comment says a benchmark enforces a 20 ms budget; the bench asserts nothing and the design's budget is 200 ms end to end. `[H]`
- F11: a doc comment names a removed function. `[H]` 1, `[R]` 6 more wrong references.
- F19: 455 `Phase N` lines, 189 dated comment lines, 72 design-doc cites in non-test `.rs`; two have already rotted (F10, F11). `[H]`

**CI and tests that do not test**
- F12: `otto cov` runs only sb's ~100 tests (`default-members = ["sb"]`, no `--workspace`) and reports 6.1%. The handoff's "cannot build" was wrong. `[H]` corrected.
- F13: doctor requires fabric patterns `create_tags` and `extract_wisdom`; nothing calls either. `[H]` 1, `[R]` 1.
- F16: 6 tests skip themselves with `eprintln!` + `return` in CI and pass. One (`candle` parity) has never asserted on any machine because its fixture was never committed. `[H]` 2, `[R]` 4.

**Smaller items**
- F15: test-only types and fns shipped `pub` to dodge `deny(dead_code)`. `[H]` 4, `[R]` 6, plus dead production code (`MultiFetcher`, `FabricFetcher`, `JinaFetcher`, `GitHubFetcher::with_token`, `orphan_notes`, `borg::serve`, a hidden `--use-mock` flag).
- F18: `.expect` on missing HOME inside `Result`-returning fns, a few lines from `ok_or_else(..)?` in the same fn. `[H]` narrowed.
- F20: `borg/src/lib.rs` mixes server startup and client commands; `fs::metadata` inside a write transaction; 2 inline test modules; `sb/src/cli/borg.rs` at 1,440/1,500 lines; release builds on Rust 1.96 while CI tests 1.98; unused `--host`/`--port` hotkey flags. `[H]`

### Goals

- Every flaw above fixed at its root, across its whole class.
- Each fix carries a test that fails on the old code, or a guard that fails on reintroduction (a script in `otto ci`, a clippy config, a golden).
- No behavior change a user would notice, except where the old behavior was the bug (named per phase).

### Non-Goals

- **F1, borg's write API open by default: excluded.** Scott, 2026-10-05: "this is local to my home ... f1 is probably left alone. but you can suggest how to close it if you want." Advisory in Addendum A.
- Converting every `vault::paths` helper to `Result` (the broad F18): rejected, Addendum B.
- New features. The one new surface, `sb cortex graph --rebuild`, exists only to roll out F5 without an LLM backfill.
- `expect_used` policy: the workspace has 143 production `expect` calls, most on regex literals. Excluded; nothing was flagged.
- Retry logic for the cortex and distillers LLM clients (no retries exist): excluded, unflagged.
- Running `otto cov` in CI: excluded. It runs the suite twice and needs a `cargo-llvm-cov` install; F12 fixes what it measures.

## Proposed Solution

### Overview

- Shared primitives move to `vault`, the one crate every other crate depends on: `vault::process` (F2), `vault::http` (F3, behind a feature), `vault::wikilink` (F5), `vault::search::filter` (F6), `vault::systemd` (F7).
- One guard script, `bin/source-lint`, called from `otto ci` and GitHub CI, enforces the mechanical rules: no self-skipping tests, no inline test modules, every crate opted into workspace lints, no `Phase N` in code or living docs, every cited `docs/design/` path resolves, matching Rust toolchains in `ci.yml` and `release.yml`.
- clippy enforces the rest: workspace `unwrap_used = "deny"`, `disallowed-methods` on `reqwest::Client::new` / `Client::builder` outside `vault::http`.

### Architecture

```
vault (new direct deps libc + reqwest[feature http]; both already in Cargo.lock, so no new crates)
  process.rs     run(Command, stdin, timeout, label) -> Exited | TimedOut     F2
  http.rs        [feature http] client(timeout), daemon_get/post               F3
  wikilink.rs    parse(body) -> WikiLink iter; stem()                         F5
  search/filter.rs  Filter builder -> (sql, Vec<Value>)                      F6
  systemd.rs     ServiceUnit spec, render_service, render_timer, user_unit_dir  F7
borg, cortex, oracle, distillers, sb  -> consume the above, delete their copies
bin/source-lint   guard script, otto ci + .github/workflows/ci.yml          F9 F16 F19 F20
clippy.toml       allow-unwrap-in-tests, disallowed-methods                 F3 F9
```

### Data Model

- `vault::process::Outcome { Exited { status, stdout: Vec<u8>, stderr: Vec<u8> }, TimedOut { after: Duration } }`. A pipe read error or a drain-thread panic is `Err`, never empty output.
- `vault::wikilink::WikiLink<'a> { target: &'a str, heading: Option<&'a str>, block: Option<&'a str>, alias: Option<&'a str>, embed: bool, span: Range<usize> }`. Rules:
  - target excludes `[ ] | #`; `#^x` -> block, `#x` -> heading
  - a `\` before `|` (table-escaped pipe) is dropped from the target
  - `embed` is a preceding `!`
  - links inside code are not links (Obsidian renders code literally) and are not yielded. Code means what `in_code_context` (`cortex/src/linking.rs:510-526`) treats as code today, which moves here: backtick and tilde fences (closing fence same character, at least the opening length), inline backtick spans of any length, and indented (4-space or tab) lines
  - `stem()` = last path segment, `.md` stripped, lowercased
- `vault::wikilink::Resolver`, built once per pass from the note set into hash indexes (stem, path), so resolution is a lookup, not a scan: `recompute_inbound_link_counts` runs under the SearchIndex mutex (`vault/src/search/stats.rs:20-24`) and must not degrade to every-link-against-every-note. Rules (Obsidian's): a target with a `/` matches the note whose lowercased path minus `.md` equals it or ends with `/` + it (component boundary: `dir/x` does not match `otherdir/x.md`); a bare target matches by stem. The live vault has 6 duplicate stems (`agents`, `readme`, `source`, 3 fabric pattern names); a bare link to a duplicated stem resolves to all of them, as the stem-based counters do today.
- `cortex/src/links.rs:114-117` keeps its extra title and slug-of-target fallbacks, layered on the resolver: they are the broken-links lint's deliberate leniency, pinned by `cortex/src/links/tests.rs`. No other consumer gains them.
- `vault::systemd::ServiceUnit` fields: description, after/wants, start-limit, unit type, env bootstrap (one `EnvBootstrap` struct replaces the copies at `borg/src/config.rs:257` and `cortex/src/config.rs:919`, same serde shape), extra env, exec args, restart policy, hardening (`Strict { rw_paths } | Minimal`), install target. Rendering is pure; callers resolve paths.

### API Design

New config keys (kebab-case, humantime durations per the 2026-10-04 queue doc's Resolved Decisions):

| Key | File | Default | Phase |
|---|---|---|---|
| `hotkey.request-timeout` | borg.yml | `10s` | F3 (replaces three hardcoded 10 s constants: `oracle/src/queue.rs:17`, `sb/src/cli/borg/wait.rs:19`, `sb/src/cli/borg.rs:17`) |
| `pipeline.github-timeout` | borg.yml | `30s` | F3 (replaces hardcoded 30 s in `borg/src/github.rs`) |
| `pipeline.browser-ua-timeout` | borg.yml | `30s` | F2/F3 (`BrowserUaFetcher` HTTP and markitdown; today it has no timeout at all) |
| `ntfy.read-timeout` | borg.yml | set by Phase 0c from the measured keepalive | F3 |

New CLI surface: `sb cortex graph --rebuild` (deterministic edges only, `build(.., force_full = true)` with no fact layer). Removed: `sb borg hotkey --host/--port` (they do nothing), hidden `sb cortex embed --use-mock`.

### Implementation Plan

32 phases in 8 tracks. One commit per phase, `otto ci` green on every commit. Cheap and deterministic work first. Each track ships as its own PR; land each before opening the next.

#### Track A: spikes (no repo code)

##### Phase 0: prove four environmental assumptions
**Model:** sonnet
- 0a, F15: in a throwaway worktree (discarded after), add a `test-util` feature to `vault` plus a self dev-dependency; confirm a release workspace build leaves it off.
- 0b, F10: time `search_vector` at 21K rows x 384 dims, debug and release, 20 runs each.
- 0c, F3: `curl -sN` the live ntfy subscription for 5 minutes; record the server keepalive interval. `ntfy.read-timeout` = 3x that.
- 0d, F2: scratch program: spawn `sh -c 'sleep 30 & head -c 1048576 /dev/zero | tr "\0" a'` in its own process group with drain threads; (1) kill the group at 2 s and confirm the drain threads return and the grandchild is gone; (2) send the scratch program itself SIGINT with a handler that kills registered groups, and confirm the grandchild is gone.
- Results get pasted into this doc's Addendum D.
- **Success criteria:**
  - `cargo tree --workspace -e normal,features -i vault@$(cargo pkgid -p vault | sed "s/.*#//") | rg test-util` prints nothing
  - Addendum D records p50/p99 and the hardware for 0b, and the keepalive interval for 0c
  - 0a and 0d PASS. A failure stops the plan: Phase 23 (needs 0a) and Phase 7 (needs 0d) do not start until this doc is revised
  - Observed on main: the `cargo tree` probe prints nothing today because no `test-util` feature exists; it only means something in the 0a worktree

#### Track B: truth in docs, CI scope, doctor (F9-F13)

##### Phase 1: comments that lie, and the budget they claim (F10, F11)
**Model:** sonnet
- `vault/src/search/vector.rs:196-202`: state the design target (scan "well under 20 ms" at 21K rows, hybrid-retrieval design doc :311), and that `otto perf` enforces a regression ceiling, not the target. Fix `vector.rs:28`.
- F10 enforcement: `vault/tests/perf.rs` gains `perf_search_vector_21k` (21K rows x 384 dims, release profile, asserts p50 under a regression ceiling: a fixed number of milliseconds, 10x the release p50 Phase 0b measured, written into the test with the hardware it was measured on, and recorded in Addendum D). New otto task `perf` runs `cargo test --release -p vault --features vec --test perf -- --ignored`. Not in `ci`: timing on shared runners at opt-level 0 is noise.
- `borg/src/intake.rs:70,74-75,115-116`: drop references to the legacy path, `record_intake`, `record_dlq`.
- Fix the six wrong references from research:
  - `borg/src/pipeline/session.rs:146-147` -> `render_note_keys_matches_the_writer`
  - `cortex/src/config.rs:277` -> `embed::in_inference_pool`
  - `borg/src/migrate.rs:10-12` -> describe current return values
  - `borg/src/notify.rs:209` -> the `_completed` / `_failed` test names
- `cortex/src/daemon.rs:175-178`, `cortex/src/opts.rs:253`: "first run after restart is a full rebuild" is false (`last_run_at` persists in SQLite, `cortex/src/graph.rs:186,250`). Fix both.
- **Success criteria:**
  - `rg -n 'enforces the budget|record_intake|record_dlq|embed::inference_pool|changed_count|matches_format_reply\b' borg/src cortex/src vault/src -g '!*tests*'` prints nothing
  - `rg -n 'no persisted' cortex/src` prints nothing
  - Observed on main: first probe hits `borg/src/migrate.rs:11`, `borg/src/intake.rs:74,116`, `borg/src/notify.rs:209`, `cortex/src/config.rs:277`, `vault/src/search/vector.rs:201`; second hits `cortex/src/opts.rs:254`, `cortex/src/daemon.rs:178`; no `otto perf` task exists
  - `otto perf` exits 0; injecting `std::thread::sleep(Duration::from_secs(1))` into `search_vector` makes it exit non-zero (recorded, reverted)

##### Phase 2: coverage measures the workspace (F12)
**Model:** sonnet
- Add otto env `CARGO_SCOPE: "--workspace --features vec"`; use it in `check`, `test`, `cov` (`.otto.yml:63,66,76,87`). Drop `--all-features`.
- Update the mirror comment in `.github/workflows/ci.yml:50`.
- **Success criteria:**
  - `otto cov` output has a `Running unittests src/lib.rs` line for each of vault, borg, cortex, oracle, distillers, sb
  - `rg -n -- '--all-features' .otto.yml` hits only comments
  - Observed on main: `otto cov` ran 3 test binaries, all sb (exit 0, 6.1% lines); the `rg` probe hits `.otto.yml:61` (comment) and `:87` (the cov command)

##### Phase 3: doctor checks the patterns that are called (F13)
**Model:** sonnet
- Two findings. Required: `summarize` (Error when missing). Fallback: the configured `intel.batch-weekly` pattern (default `weekly_digest`, called at `cortex/src/intel.rs:509-513`), its own finding at Warn when missing.
- Split the stdout parsing out of `fabric_default_patterns_findings` (`sb/src/cli/checks.rs:362`) so it is unit-testable.
- Delete dead `IntelConfig.fabric_patterns` and `on_new_note` (`cortex/src/config.rs:706,710`; read nowhere).
- `README.md:40`: drop `create_tags`.
- **Success criteria:**
  - `rg -n 'create_tags|extract_wisdom' --type rust` prints nothing
  - new tests on the split-out parser: stdout with `summarize` and the configured weekly pattern -> two Ok findings; without `summarize` -> Error; without the weekly pattern -> Warn naming it; a non-default `batch-weekly` value is the one checked
  - Observed on main: probe hits `sb/src/cli/checks.rs:374,378`, `cortex/src/config.rs:730,732`

##### Phase 4: `unwrap_used` at workspace level (F9)
**Model:** sonnet
- `Cargo.toml`: `[workspace.lints.clippy] unwrap_used = "deny"`; rewrite the comment at `:12-15`.
- `clippy.toml`: `allow-unwrap-in-tests = true`.
- Delete the per-crate `#![deny(clippy::unwrap_used)]` lines (borg, vault, distillers, sb lib + main, strip-transcripts). Keep the existing `#![allow]`s in integration-test helpers.
- **Success criteria:**
  - `cargo clippy --workspace --all-targets --features vec -- -D warnings` exits 0
  - planting `let _ = Some(1).unwrap();` in a non-test fn in `cortex/src/lib.rs` makes it exit non-zero with `unwrap_used` (recorded in implementation notes, then reverted)

#### Track C: tests that test, and the guard (F16, F19, F20 inline modules)

##### Phase 5: no silent skips (F16)
**Model:** opus
- `cortex/src/proposals/tests.rs:665`, `cortex/src/sweep/tests.rs:847`: build the precondition with a regular file used as a directory (ENOTDIR holds as root). Delete the chmod and the skip.
- `borg/src/youtube/tests.rs:289`: add `ffmpeg` to the apt line in `.github/workflows/ci.yml:26`; missing ffmpeg panics.
- `borg/src/slides/cleanup/tests.rs:128`: `#[ignore = "needs rkvr on PATH"]`; a missing rkvr panics when the test is run.
- `vault/src/embedding/candle/tests.rs:38`: `#[ignore = "downloads bge-small (~133 MB)"]` replaces the `CANDLE_TESTS_REAL` check.
- `vault/tests/regression/candle/parity.rs`: generate and commit `vault/tests/fixtures/bge-reference.json` with `pipx run --spec sentence-transformers python bin/gen-bge-reference.py`; a missing fixture panics; the test is `#[ignore = "downloads bge-small (~133 MB)"]`.
- New otto task `ignored` runs the three local-only tests by exact name: `cargo test --workspace --features vec -- --ignored --exact <candle pool test> <candle parity test> slides::cleanup::tests::test_cleanup_orphans_end_to_end` (full paths fixed in the task). One documented entry point instead of an env var; the four pre-existing ignored tests are not in it.
- `vault/src/fabric/tests.rs:45,65` (sh missing -> skip): moves to `vault::process` tests in Phase 7, where a missing `sh` panics.
- Delete the two assertion-free tests: `borg/src/extraction/tests.rs:13-15`, `cortex/src/fabric/tests.rs:5`.
- **Success criteria:**
  - in the GitHub CI job (runs as root in `debian:bookworm`) the two rewritten tests report `ok` and the log has no `skipping` line
  - a mutation mapping every staging-root metadata error to `Ok(StagedScan::default())` makes both fail locally (recorded, reverted)
  - `otto ignored` on desk exits 0 and its output shows `3 passed` (recorded); the committed fixture parses and holds one vector per reference sentence
  - `rg -n -U -i 'eprintln!\(\s*"[^"]*skip' -g '*tests*' -g '**/tests/**' borg cortex vault distillers oracle sb` prints only `vault/src/fabric/tests.rs` (Phase 7 removes it). `-U` is required: the candle and parity skips span two lines. The test-file globs are required: `sb/src/cli/borg.rs:1236-1279` prints legitimate "skipped" status lines to users.
  - Observed on main: 10 lines across 8 sites: `vault/src/embedding/candle/tests.rs:39-40`, `borg/src/youtube/tests.rs:290`, `borg/src/slides/cleanup/tests.rs:129`, `cortex/src/proposals/tests.rs:667`, `vault/src/fabric/tests.rs:46,66`, `cortex/src/sweep/tests.rs:849`, `vault/tests/regression/candle/parity.rs:50-51`

##### Phase 6: `bin/source-lint`, first checks (F9 guard, F16 guard, F20 inline modules, F20 toolchain)
**Model:** sonnet
- Extract inline test modules to sibling files: `vault/src/tombstone.rs:41`, `cortex/src/report.rs:229`.
- `bin/source-lint` (bash; every `rg` gets explicit paths, since bare `rg` reads stdin and hangs; exits 2 naming the tool if `rg` is not on PATH). Add `ripgrep` to the apt line in `.github/workflows/ci.yml:26-27` (not installed today). Checks, each printing `file:line` and exiting 1:
  - any `eprintln!(".*skip` in test code (multiline, test files only: the Phase 5 probe)
  - any `#[cfg(test)]` followed by an inline `mod x {`
  - any workspace member `Cargo.toml` without `[lints]\nworkspace = true`
  - `RUST_VERSION:` differs between `ci.yml` and `release.yml`
- Set `release.yml` to `1.98.0`; rewrite the `ci.yml:9-12` comment.
- Wire into `.otto.yml` as task `source-lint` in `ci.before`, and as a step in `ci.yml` beside `bin/agents-map` (precedent `.otto.yml:50-53`, `ci.yml:97-100`).
- **Success criteria:**
  - `bin/source-lint` exits 0
  - each planted violation (one per check) makes it exit 1 naming the file; with `rg` removed from PATH it exits 2
  - Observed on main: `bin/source-lint` does not exist; `RUST_VERSION` is `1.98.0` in `ci.yml:13`, `1.96.0` in `release.yml:10`

#### Track D: subprocess and HTTP (F2, F3, F22)

##### Phase 7: `vault::process` (F2)
**Model:** opus
- New `vault/src/process.rs` + `process/tests.rs`. `run(cmd: Command, stdin: Option<Vec<u8>>, timeout, label) -> Result<Outcome>`:
  - sets all three pipes itself, so a caller cannot forget to drain
  - spawns in its own process group (`CommandExt::process_group(0)`); on timeout kills the group, so a grandchild (fabric -> yt-dlp) cannot hold the pipe open
  - feeds stdin and drains stdout and stderr on their own threads
  - read errors and thread panics are `Err`
  - DEBUG on entry (label, timeout), DEBUG on exit (status, byte counts), WARN on timeout
  - Ctrl-C: the child's own process group no longer receives the terminal's SIGINT. `vault::process` registers each live group id; `vault::process::kill_registered()` SIGKILLs them. sb installs a SIGINT/SIGTERM handler that calls it and exits 130, for every command except the long-running daemons (`borg daemon --start`, `cortex daemon`), which keep their graceful-shutdown handlers and are covered by systemd's control-group kill. `PR_SET_PDEATHSIG` was considered and rejected: it reaches only the immediate child, and a SIGKILLed child cannot pass it on to a grandchild.
- The group kill needs `libc` as a direct dependency (`cargo add libc -p vault`; already in the lockfile).
- Port `vault::fabric` onto it; delete `vault::fabric::wait_with_timeout`.
- Tests use the `head -c N /dev/zero | tr '\0' a` form; bare `head -c` splices into the pipe on this kernel and passes against the buggy loop.
- **Success criteria:**
  - `cargo test --package vault --lib process` passes: 1 MiB stdout, 1 MiB stderr, 1 MiB stdin through `cat`, each under 5 s with a 30 s timeout; timeout kills a grandchild; `kill_registered` kills a grandchild of a live call
  - a harness process that installs the handler, runs a call whose child backgrounds a grandchild, and receives SIGINT leaves no grandchild alive
  - swapping `run` for a poll-without-drain loop makes the three 1 MiB tests fail (recorded in implementation notes)
  - `rg -n 'fn wait_with_timeout\(' vault/src` prints nothing (borg's copy goes in Phase 8)
  - Observed on main: `vault/src/fabric.rs:187`

##### Phase 8: migrate every spawn site (F2)
**Model:** sonnet
- Six sites move to `vault::process::run`, each split into "build the `Command`" + "run it" so tests can substitute `sh -c`:
  - `borg/src/fabric.rs` `fetch_transcript` (:77), `fetch_article_blocking` `fabric -u` (:133) and markitdown fallback (:163)
  - `borg/src/extraction.rs:18`, `borg/src/ocr.rs:86`
  - `borg/src/stages/fetcher.rs:252` `BrowserUaFetcher` markitdown, bounded by `pipeline.browser-ua-timeout` (the key is added in this phase; Phase 10 reuses it for the same fetcher's HTTP call)
- Every timeout becomes `Err` with its label. Callers already WARN and degrade (`handlers.rs:102-110,666,1138-1150`, `slides.rs:502`); `fetch_article_blocking` gains the WARN it lacks.
- Delete `borg/src/fabric.rs:8-32`, dead `MultiFetcher` / `FabricFetcher` / `JinaFetcher` (`borg/src/stages/fetcher.rs:31,100,166`; no constructor is called anywhere).
- Replace the two tests that re-implement the loop instead of calling production code: `borg/src/ocr/tests.rs:14-40`, `borg/src/pipeline/timeouts.rs:73-100`.
- **Success criteria:**
  - `rg -n 'try_wait' --type rust -g '!vault/src/process.rs' -g '!*tests*' -g '!**/tests/**'` prints nothing (`sb/tests/wait.rs:168` polls the sb binary with no piped output and stays)
  - Observed on main: `vault/src/fabric.rs:180,221`, `borg/src/extraction.rs:29`, `borg/src/ocr.rs:103`, `borg/src/fabric.rs:19`, plus test copies `borg/src/ocr/tests.rs:29`, `borg/src/pipeline/timeouts.rs:88`
  - one 1 MiB-output test per migrated site passes in `cargo test --package borg --lib`
  - `rg -n 'fn wait_with_timeout\(' borg/src` prints nothing (observed on main: `borg/src/fabric.rs:15`)

##### Phase 9: one HTTP client constructor, daemon clients (F3, F22)
**Model:** sonnet
- `vault` feature `http` (reqwest, already at 0.13.5); `vault::http::client(timeout) -> Result<Client>`. borg and oracle enable it.
- One daemon-request helper beside `vault::daemon`: resolves address, token, `hotkey.request-timeout`; checks status before parsing, so a 401 reads "daemon rejected the request (401)", not "Failed to parse response" (`borg/src/lib.rs:922`).
- Route through it: `borg/src/lib.rs:828,902`, `borg/src/replay.rs:259,291`, `borg/src/migrate.rs:405` (gains the bearer: F22), `borg/src/queue.rs:487`, `oracle/src/queue.rs:50`. Delete the three 10 s constants.
- **Success criteria:**
  - per entry point, a silent-listener test returns `Err` within 2x the timeout; a listener that sends headers and stalls the body also returns `Err` within 2x
  - a `hotkey.request-timeout: 1s` config is the timeout used (a 2 s stall fails, a 0.5 s delay succeeds)
  - a stub answering 401 with a non-JSON body yields an error naming the 401, not a parse error
  - `sb borg wait` still caps each request at `min(request-timeout, time left)` (`sb/src/cli/borg/wait.rs:95`); its existing exit-5 test passes
  - a token-requiring stub accepts `reingest-failed`'s request
  - `rg -n 'const \w*REQUEST_TIMEOUT' sb/src oracle/src` prints nothing
  - Observed on main: `oracle/src/queue.rs:17`, `sb/src/cli/borg.rs:17`, `sb/src/cli/borg/wait.rs:19`

##### Phase 10: fetchers, ntfy stream, and the guard (F3)
**Model:** opus
- `vault::http` exposes `builder(Timeouts) -> ClientBuilder` (callers add user agent and redirect policy) and `client(Timeouts) -> Result<Client>`. `Timeouts::total(d)` for request/response calls; `Timeouts::stream { connect, read }` for ntfy, which must not have a total timeout.
- Migrate every remaining construction to it, not just the timeout-less ones: `borg/src/youtube.rs:14`, `borg/src/ocr.rs:175`, `borg/src/readability.rs:43`, `borg/src/transcription.rs:36`, `borg/src/jina.rs:34`, `borg/src/github.rs:288,303`, `borg/src/stages/fetcher.rs:210`, `borg/src/ntfy.rs:103`.
- `GitHubFetcher::new` and `BrowserUaFetcher::new` return `Result`. `borg/src/jina.rs:22` propagates. `distill_for_publish_repo` (`borg/src/stages/distill.rs:590`) returns `Distilled`, not `Result`, by design: a construction error takes its existing `fallback_distilled` path with a WARN. Timeouts from `pipeline.github-timeout`, `pipeline.browser-ua-timeout`. Delete `GitHubFetcher::with_token` (no callers).
- ntfy (`borg/src/ntfy.rs:103`): build the client once with `connect_timeout` + `read_timeout` (`ntfy.read-timeout`); a read error logs WARN and reconnects through backoff instead of ending the loop silently (`:126`).
- `clippy.toml` `disallowed-methods`: `reqwest::Client::new`, `reqwest::Client::builder`, `reqwest::ClientBuilder::new`; `#[allow]` only inside `vault::http`. Test code builds its clients through `vault::http::client` too, so there are no test exemptions.
- **Success criteria:**
  - `rg -n 'Client::new\(\)|falling back to default client' --type rust` prints nothing
  - clippy fails on a planted `reqwest::Client::new()` in borg (recorded, reverted)
  - Observed on main: the `rg` probe prints 14 lines
  - an ntfy stub that sends headers then stalls is reconnected at least twice within 3x `read-timeout` plus the backoff delays (1 s, 2 s)
  - an ntfy stub that sends a keepalive every `read-timeout / 3` causes zero reconnects over 3x `read-timeout`
  - `rg -n 'Client::builder\(\)|ClientBuilder::new' --type rust -g '!vault/src/http.rs'` prints nothing
  - Observed on main: 9 lines: `oracle/src/queue.rs:50` (Phase 9), `borg/src/transcription.rs:36`, `borg/src/ocr.rs:175`, `borg/src/readability.rs:43`, `borg/src/github.rs:288,303`, `borg/src/youtube.rs:14`, `borg/src/stages/fetcher.rs:210`, `borg/src/jina.rs:34`

#### Track E: errors that lose state, backoff, HOME (F4, F8, F18)

##### Phase 11: read-modify-write fails closed, panics counted (F4 top tier)
**Model:** sonnet
- A file that fails to parse is never written back:
  - `cortex/src/entities.rs:186`, `cortex/src/bridge.rs:308`: return `Err` naming the path
  - `borg/src/stages/raw.rs:236` (inside Gate-1's rejection path): skip the blocklist update with a WARN naming the path. The capture is still rejected exactly as today (`run_gate_1` returns `Err`); only the write-back is skipped, and the rejection's `blocklist_updated` metadata is `false` when it was
- Gate-0 (`borg/src/stages/raw.rs:154-157`): a blocklist that fails to load is treated as empty with a WARN, so every domain passes. Phase 11 makes this last longer: today the next Gate-1 rejection overwrites the corrupt file and restores enforcement; after this phase the corruption persists. Gate-0 fails closed: a load error rejects the URL capture with an error naming the blocklist path, emits a gate alert (`alert::emit_gate_alert`, same cooldown as a block), and records the receipt `failed` / `intake-rejected` (`borg/src/pipeline.rs:250`), so `sb borg replay` recovers it once the file is repaired. A missing file is still an empty blocklist (`Blocklist::from_file`, `borg/src/blocklist.rs:60-62`).
- `cortex/src/summarize.rs:189-191`: a `JoinError` counts as failed and WARNs with the panic. The semaphore-closed path (:155-160) returns before `attempted` is incremented (:163), so it increments both `attempted` and `failed`.
- `cortex/src/summarize.rs:166`: checkpoint failure WARNs with the path.
- `cortex/src/naming.rs:256-259`: keep going, collect unreadable paths, WARN each, return them to the three callers (`naming.rs:209`, `classify.rs:610`, `migrate.rs:225`), which report them. Propagating mid-loop would leave a half-done rewrite after the renames already happened.
- `borg/src/pipeline/handlers.rs:354`: manifest write failure WARNs (nothing reads `slides.yml` back).
- **Success criteria:**
  - a corrupt proposals file or bridge file makes the write return `Err`; a corrupt blocklist leaves Gate-1 logging WARN; in all three the file's bytes are unchanged
  - backfill with one panicking task reports `attempted == distilled + skipped + failed` and `failed == 1`; same invariant with the semaphore closed mid-run
  - a corrupt blocklist: Gate-1 still rejects the capture and records `blocklist_updated: false`
  - a corrupt blocklist: Gate-0 rejects a URL capture, the receipt is `failed` / `intake-rejected` with the blocklist path in its reason, and one gate alert fires; a missing blocklist still lets the capture through
  - a rename with one unreadable linking note returns that path, and the caller's report names it
  - `rg -n 'let _ = (save_checkpoint|h\.await|slides::write_manifest)' --type rust` prints nothing
  - Observed on main: `cortex/src/summarize.rs:166,190`, `borg/src/pipeline/handlers.rs:354`

##### Phase 12: remaining swallowed errors (F4)
**Model:** sonnet
- WARN with path or id: `borg/src/migrate.rs:30`, `borg/src/stages/artifact.rs:230,339`, `borg/src/lib.rs:778`, `borg/src/discord.rs:244,321`, `borg/src/telegram.rs:120`, `borg/src/service.rs:62,327,333,354`, `sb/src/cli/bootstrap/migrate.rs:120`, `cortex/src/entities.rs:172`.
- `vault/src/search/stats.rs:560-563` (`note_quality`): `.optional()?` instead of `.ok()` (precedent `borg/src/receipts.rs:689`).
- Every legitimate `let _ =` (kill after timeout, rollback on a propagating error, temp cleanup, unlock, shutdown sends; full list in research) gets nothing: the reason is visible from the line.
- **Success criteria:**
  - `rg -n -U 'query_row\([^;]*?\.ok\(\)' vault/src borg/src cortex/src oracle/src` prints nothing (`-U`: the call spans lines)
  - Observed on main: one match, `vault/src/search/stats.rs:560` (`note_quality`)
  - a test per site that can be driven locally (`borg/src/migrate.rs:30`, `borg/src/stages/artifact.rs:230,339`, `borg/src/lib.rs:778`, `cortex/src/entities.rs:172`, `sb/src/cli/bootstrap/migrate.rs:120`, `vault/src/search/stats.rs:560`) asserts the WARN or the propagated error
  - the sites whose failure needs a live external service (`borg/src/discord.rs:244,321`, `borg/src/telegram.rs:120`, `borg/src/service.rs:62,327,333,354`) are listed in implementation notes with the WARN line quoted; no test, because faking Discord, Telegram, and systemctl failures costs more than the one-line WARN it would cover

##### Phase 13: one backoff (F8)
**Model:** sonnet
- `borg/src/backoff.rs`: constructor taking `(base, cap)`; pure `next_delay(hint: Option<Duration>)` that caps the hint and advances the attempt count either way; `wait` takes a label instead of the hardcoded "reconnecting".
- Retry-After header parsing as `retry_after_header`, both forms: delta-seconds and HTTP-date (via `httpdate`, already in the lockfile; `cargo add httpdate -p borg`). Today the HTTP-date form is silently ignored (`borg/src/transcription.rs:206-211`). The name `parse_retry_after` is taken by an unrelated blocklist fn (`borg/src/blocklist.rs:167`).
- `borg/src/transcription.rs` uses it; delete `retry_backoff` and `GROQ_BACKOFF_CAP`. Same schedule (1 s, 2 s, 4 s; cap 20 s).
- ntfy calls `reset_if_healthy` like its three siblings (`borg/src/ntfy.rs:148`).
- **Success criteria:**
  - `rg -n 'fn retry_backoff|GROQ_BACKOFF_CAP' borg/src` prints nothing
  - a 3600 s hint is capped at 20 s, and the attempt count advances after a hinted delay
  - `retry_after_header` parses `120` and `Wed, 21 Oct 2026 07:28:00 GMT` (relative to a fixed now); garbage -> `None`
  - ntfy: a message within the healthy window resets the backoff via `reset_if_healthy`; a test pins it
  - Observed on main: probe hits `borg/src/transcription.rs:17,230,231,233,238` and `borg/src/transcription/tests.rs:35,37`

#### Track F: shared parsers, builders, renderers (F5, F6, F7, F18)

##### Phase 14: `vault::wikilink` (F5)
**Model:** opus
- New ungated `vault/src/wikilink.rs` (not under `search`, which is feature-gated and which borg does not enable). Add it to `vault/AGENTS.md` (`bin/agents-map` checks).
- Table tests, each with its expected result:

| Input | Yields |
|---|---|
| `[[a]]` | target `a` |
| `[[a\|b]]` | target `a`, alias `b` |
| `[[a#h]]` | target `a`, heading `h` |
| `[[a#^x]]` | target `a`, block `x` |
| `[[a#h\|b]]` | target `a`, heading `h`, alias `b` |
| `![[e]]` | target `e`, embed |
| `[[a\\\|b]]` (table-escaped) | target `a`, alias `b` |
| `[[dir/a]]` | target `dir/a`, stem `a` |
| `[[a.md]]` | target `a.md`, stem `a` |
| `[[#h]]` | target empty, heading `h` (a same-note link) |
| `[[a\|b\|c]]` | target `a`, alias `b\|c` |
| `[[a [[b]] c]]` | target `b` only |
| `[[]]`, `[[a` (unclosed), `[[\|b]]` | nothing |
| link inside a backtick fence, a tilde fence, an inline span (1 and 2 backticks), an indented line | nothing |
| byte spans | `span` covers `[[` through `]]`, plus the `!` for an embed |
- No callers change in this phase.
- **Success criteria:**
  - `cargo test -p vault --lib wikilink` passes
  - every row of the table above is a passing test case

##### Phase 15: migrate the readers (F5)
**Model:** sonnet
- `vault::search::extract_wikilinks` and its consumers (`find_outbound_links`, `find_inbound_links` incl. its `LIKE` prefilter, `recompute_inbound_link_counts`, `cortex/src/graph.rs:281`), `cortex/src/quality.rs`, `cortex/src/links.rs`, `cortex/src/linking.rs`, `borg/src/dedupe.rs`. Comparison goes through `Resolver`.
- `find_inbound_links` prefilter: `LIKE '%[[{stem}%'` misses `[[dir/{stem}]]`; widen to `LIKE '%{stem}%'` and let `Resolver` decide.
- Delete dead `orphan_notes` (`vault/src/search/query.rs:299`; no caller, no test).
- Regression tests: a `[[dir/x]]` link counts as inbound in `find_inbound_links` and in quality; `[[tomb#h]]` blocks the dedupe archive; `[[a#h]]` is not a broken link; `[[dir/x]]` does not credit `otherdir/x.md`; a link inside inline code is not counted by quality or linking; links.rs title and slug resolution tests still pass unchanged.
- **Success criteria:**
  - those four tests pass and fail on `01408d3` (recorded)
  - `rg -n -F '\[\[' --type rust -g '!*tests*' -g '!**/tests/**' -g '!vault/src/wikilink.rs'` prints only `cortex/src/unlink.rs:30` and `cortex/src/naming.rs:275` (Phase 16 removes those)
  - Observed on main: the probe prints 7 lines: `vault/src/search.rs:24`, `cortex/src/linking.rs:84`, `cortex/src/unlink.rs:30`, `cortex/src/links.rs:11`, `cortex/src/quality.rs:13`, `cortex/src/naming.rs:275`, `borg/src/dedupe.rs:69`

##### Phase 16: migrate the writers and string parsers (F5)
**Model:** opus
- `cortex/src/unlink.rs`: use `parse`; explicitly skip links with a heading or block (pinned by `cortex/src/unlink/tests.rs:220-222`); keep the embed and code checks (code-context detection moves to `vault::wikilink`).
- `cortex/src/naming.rs:275`: rewrite by span instead of compiling a regex per stem.
- String parsers: `vault/src/ledger.rs:196`, `cortex/src/association.rs:1185`, `oracle/src/eval/calc.rs:10`, `cortex/src/linking.rs:127`.
- Add `sb cortex graph --rebuild`: rebuild the deterministic edge kinds only. Today `build(force_full = true)` calls `clear_edges()` (`cortex/src/graph.rs:208`), which runs `DELETE FROM edges` (`vault/src/search/graph.rs:234-236`) and so also deletes fact edges (`kind = 'fact'`, written by `cortex/src/memgraph.rs:123-124`). `--rebuild` instead deletes only the deterministic kinds and re-inserts them inside one transaction; any error rolls back and leaves the previous edges intact. `--backfill` keeps its current behavior (it regenerates facts itself). Rollout step: run it once after deploy.
- **Success criteria:**
  - `rg -n -F '\[\[' --type rust -g '!*tests*' -g '!**/tests/**' -g '!vault/src/wikilink.rs'` prints nothing
  - `cargo test --package cortex -- unlink naming` passes, including `unlink/tests.rs:220` (amended during execution: cortex has no `vec` feature, so the original `--features vec` form errors `the package 'cortex' does not contain this feature: vec`; reproduced on the branch at f21c699)
  - `--rebuild` on an index seeded with fact edges keeps the fact-edge count and content unchanged and rebuilds wikilink edges
  - an injected insert failure during `--rebuild` leaves the edge table byte-identical to before
  - the four string-parser call sites each have a test exercising a heading or path-form link

##### Phase 17: one SQL filter builder (F6)
**Model:** sonnet
- `vault/src/search/filter.rs`: `Filter { sql, params: Vec<rusqlite::types::Value> }` with `and_eq`, `and_cmp`, `and_tags(alias, tags, all)`; bare positional `?` everywhere, base clauses included (`MATCH ?`, `model_version = ?`), so no numbered placeholder survives to mix with them (precedent `borg/src/receipts.rs:699-735`). `push_tags_filter` (`query.rs:13-44`) folds in.
- Migrate the 7 sites: `query.rs:76,157`, `stats.rs:306,418,468,609`, `vector.rs:220`.
- Unit tests assert exact SQL and params; per public query, an all-filters-set positive and negative test.
- **Success criteria:**
  - `rg -n 'param_idx' vault/src` prints nothing
  - Observed on main: 40 lines
  - `cargo test -p vault --features vec` passes

##### Phase 18: golden units (F7)
**Model:** sonnet
- Byte-exact goldens for borg, cortex, and harvest service and timer units at fixed inputs, against current code.
- **Success criteria:** the goldens pass on the unmodified renderers

##### Phase 19: `vault::systemd` (F7)
**Model:** opus
- Move rendering to `vault/src/systemd.rs` (spec in Data Model), byte-identical to the goldens. One `EnvBootstrap`. Pure rendering: cortex stops calling `cortex_config().exists()` and `xdg_data_dir()` inside the renderer (`cortex/src/daemon.rs:884-892`), harvest stops calling `borg_config().exists()` (`borg/src/harvest/timer.rs:53`); callers pass them in. Add to `vault/AGENTS.md`.
- `Restart=always` (borg) vs `on-failure` (cortex) stays a parameter: neither has a recorded reason, and changing restart behavior is not a refactor.
- **Success criteria:**
  - goldens unchanged
  - `rg -n 'mise/shims' --type rust -g '!*tests*'` prints exactly 1 line
  - Observed on main: 3 lines: `borg/src/service.rs:218`, `cortex/src/daemon.rs:914`, `borg/src/harvest/timer.rs:89`

##### Phase 20: unit fixes, HOME consistency, drift check (F7, F18)
**Model:** sonnet
- borg: pin `--config` like cortex and harvest; unit dir from `vault::paths::xdg_config_dir()` (`borg/src/service.rs:244,318`); drop the fabricated vault fallback `home.join("repos/scottidler/obsidian")` (`service.rs:252-254`) for an error.
- F18 narrowed: inside `Result` fns use `ok_or_else(..)?`: `cortex/src/daemon.rs:955,991,1027`, `borg/src/harvest/timer.rs:123,156`; `borg/src/blocklist.rs::default_path` returns `Result` (precedent `borg/src/receipts.rs:794`). Fix the precondition wording in `CLAUDE.md:58` and `vault/AGENTS.md:41`: `dirs` falls back to the passwd entry, so `None` needs HOME unset AND no passwd entry.
- `sb doctor`: Warn when an installed unit's bytes differ from the current render, naming the reinstall command. A unit that is not installed (lappy runs no daemon) is not drift and reports nothing. Doctor renders with the same inputs `--install` uses, through the same fn. This is the guard for the `cortex.service` drift.
- Update goldens in this commit.
- **Success criteria:**
  - `rg -n 'expect\("xdg_' cortex/src/daemon.rs borg/src/harvest/timer.rs borg/src/blocklist.rs` prints nothing
  - doctor reports drift for a unit file edited by one byte, nothing for a fresh install, and nothing when the unit is absent
  - with `XDG_CONFIG_HOME` set to a tempdir, the borg unit is written under it
  - the rendered borg unit's `ExecStart` carries `--config <path>`
  - Observed on main: probe hits `cortex/src/daemon.rs:886,955,991,1027`, `borg/src/harvest/timer.rs:123,156`, `borg/src/blocklist.rs:130` (`:886` goes in Phase 19 when the renderer turns pure)

#### Track G: oracle, gating, borg layout (F14, F15, F17, F20, F21, F23)

##### Phase 21: oracle reads the configured vocabulary, fails loud (F14)
**Model:** sonnet
- oracle reads `tags.canonical-path` from borg.yml through its existing `BorgView` (`oracle/src/queue.rs:21-40`): one key, the same file on every host. No new oracle.yml key. Key unset -> `vault::paths::canonical_tags()`, borg's own default, so the two cannot disagree. The path is tilde-expanded. borg.yml absent -> the default; present but unreadable or unparseable -> the tool reports that error (checked with `fs::metadata` error kind, not `Path::exists()`, which cannot tell absent from unreadable).
- borg `TagsConfig.canonical_path` becomes a tilde-expanded `PathBuf` (`borg/src/config.rs:1159`; violates the CLAUDE.md invariant today). `borg/src/startup.rs:80` validates the configured path, not the default.
- `load_canonical_tags(path) -> Result`. `schema_info` returns `tags: null` and `vocabulary-error: "<path>: <err>"`; `tag_brief` returns `known: null` with the error, never `false`.
- `sb doctor`: Warn when borg's `tags.canonical-path` and cortex's `sweep.canonical-path` differ.
- Oracle tests point at a tempdir fixture instead of reading `~/.config/sb/` (`oracle/src/server/tests.rs:748,1016`).
- **Success criteria:**
  - a borg.yml fixture setting `tags.canonical-path: ~/<tempdir-relative>/tags.yml` (tilde form) holding `zz-fixture-only`: `schema_info` lists it
  - a nonexistent path -> `schema_info` names it, `tag_brief{"tag":"x"}` returns `known: null`; an unparseable borg.yml -> the error names borg.yml
  - doctor Warns when borg's and cortex's canonical paths differ, and is silent when they match
  - `load_canonical_tags` takes the path as a parameter; the only `vault::paths::canonical_tags()` reference left in oracle is the `BorgView` default
  - Observed on main: probe hits `oracle/src/server.rs:1173`

##### Phase 22: `limit` above `top_k` (F17)
**Model:** sonnet
- Per call, retrieve depth = `max(method.top_k, limit)` for each enabled method and for the legacy `K_RRF_INPUT` seeds (`oracle/src/server/pipeline.rs:211-219,338,345,469`; `vault/src/search/vector.rs:1054`).
- The `limit` schema description says it is an upper bound: the exclude stage (`oracle/src/server/pipeline.rs:383-390`) can still return fewer when stubs are dropped.
- **Success criteria:**
  - 60 bm25-matching notes, `bm25_only_retrieval`, `limit=100` -> 60 results (50 on `01408d3`)
  - `limit=10` result order identical before and after
  - the same 60-match check passes for vector-only (mock embedder), configured hybrid, configured graph, and legacy `mode=hybrid` / `mode=graph`

##### Phase 23: test-only code behind `test-util` (F15)
**Model:** sonnet
- `#[cfg(any(test, feature = "test-util"))]`, with dev-dependencies enabling it: `FakeFabric` (distillers), vault's `insert_test_note_row`, `insert_test_note_full`, `set_test_*`, `delete_note_for_test`, `MockEmbedder`, `MockReranker`.
- `#[cfg(test)]` (single-crate use): `MemArtifactStore`, `FakeTagClassifier`, both `MockJudge`s, `cortex::testutil`.
- Delete `sb cortex embed --use-mock` (`sb/src/cli/cortex.rs:420`, `cortex/src/opts.rs:217`, `cortex/src/embed.rs:210-212`).
- Delete `group_by_slug` (`cortex/src/association.rs:64`); move its 2 fixture users to `write_session_file_with_trace`; port its 3 uncovered invariants (non-session note excluded, empty input, deterministic order) onto `group_by_session_identity`.
- **Success criteria:**
  - `cargo build --release --workspace` passes, and `cargo tree --workspace -e normal,features -i vault@$(cargo pkgid -p vault | sed "s/.*#//") | rg test-util` prints nothing
  - `rg -n group_by_slug --type rust` prints nothing
  - Observed on main: 13 lines

##### Phase 24: hotkey and launchd commands (F21, F20 unused params)
**Model:** sonnet
- Bind `{exe} borg ingest --clipboard` (`borg/src/service.rs:374`); plist runs `{exe} borg daemon --start` (`:292`).
- Drop `host`/`port` from `install_hotkey` and the `--host`/`--port` flags (`sb/src/cli/borg.rs:264-269`); the banner prints the bound command and the configured `hotkey` address.
- User-facing strings still naming the retired `obsidian-borg` binary: `sb/src/cli/borg.rs:485,807-808`, `borg/src/migrate.rs:441`. Identity strings stay, because renaming them orphans what is already installed: the extension id `obsidian-borg@scottidler` (`borg/src/extension/install.rs:16`), the launchd label and log paths (`borg/src/service.rs:278-300,345`), and the GNOME keybinding dconf path (`borg/src/service.rs:366`).
- Test: the bound command and the plist args parse with sb's own clap.
- Rollout step: `sb borg hotkey --install` on desk (the live binding points at a missing `obsidian-borg` binary).
- **Success criteria:**
  - the clap-parse test passes, and fails on `01408d3`'s `ingest --clipboard`
  - `rg -n 'obsidian-borg' sb/src borg/src/migrate.rs` prints nothing
  - Observed on main: `sb/src/cli/borg.rs:485,807,808`, `borg/src/migrate.rs:441`

##### Phase 25: extension options page (F23)
**Model:** sonnet
- `borg/clients/extension/options.js:19`: origin from `url.hostname`. `IngestRequest` is unchanged, so the additive-only rule does not fire; this is a re-sign for a JS fix.
- **Success criteria:**
  - `rg -n 'url\.host\}' borg/clients/extension` prints nothing
  - `sb borg extension stage --to "$TMPDIR/ext"` succeeds and the staged `options.js` uses `hostname`
  - rollout check (recorded): on lappy, the options page saves `http://desk.lan:8181`, the stored endpoint keeps `:8181`, a capture lands, and an origin outside `extension.origin-patterns` is still refused
  - Observed on main: probe hits `borg/clients/extension/options.js:20`

##### Phase 26: split `borg/src/lib.rs` (F20)
**Model:** opus
- Move only. `borg/src/server.rs` (AppState, `build_router`, startup types, `serve_init`) + `server/transports.rs` (one start fn per subsystem, each returning its status); `borg/src/client.rs` (note, ingest_file, reingest, ingest). Re-export `borg::serve_init`. Delete dead `borg::serve` (`lib.rs:485`, no caller). Update `borg/AGENTS.md`.
- **Success criteria:**
  - `wc -l borg/src/lib.rs` is under 300 (observed on main: 993)
  - `otto ci` exits 0 and `bin/agents-map` passes

##### Phase 27: index mtime, CLI split, duplicate helper (F20)
**Model:** sonnet
- `vault/src/search/index.rs`: compute mtimes after `scan_vault`, before `BEGIN IMMEDIATE`; one `file_mtime_secs` helper shared with `index_changed` (`:400-403`). A metadata error no longer becomes mtime `0`: that note is skipped for this pass (its existing row is kept, not upserted and not removed as stale), WARN with the path, counted in `IndexStats`.
- Split `sb/src/cli/borg.rs` at its seams: `borg/args.rs` (23-418), `borg/render.rs` (789-1310), `borg/tools.rs` (1311-1440).
- `sb/src/cli/checks.rs:641` `human_bytes` -> `vault::rss::human_bytes`.
- **Success criteria:**
  - `rg -n 'fs::metadata' vault/src/search/index.rs` prints exactly 1 line (inside `file_mtime_secs`), and in the full-reindex fn every `file_mtime_secs` call precedes `BEGIN IMMEDIATE`
  - a note whose metadata read fails keeps its existing row and appears in the skipped count
  - `wc -l sb/src/cli/borg.rs` is under 600; `rg -n 'fn human_bytes' sb/src` prints nothing
  - Observed on main: `fs::metadata` at `index.rs:35` (inside the transaction) and `:400`; `sb/src/cli/borg.rs` 1440 lines; `fn human_bytes` at `sb/src/cli/checks.rs:641`

#### Track H: comment archaeology (F19), last so code moves are done

##### Phase 28: strip narration, borg (F19)
**Model:** sonnet
- Rubric, per comment (applies to Phases 28-30):
  - `Phase N` tag: delete the tag, keep the sentence if it says why
  - date: keep only as scar-tissue evidence (with what broke, a SHA, version, PR, or an external observation); drop bare attributions like "(2026-07-24 design, panel finding 8)"
  - `docs/design/` cite: keep when it points at the reason for an invariant
- Scope: `borg/` (tests included). Rewrite user-facing `borg/src/replay.rs:445` ("staged before Phase 7") to the condition it means.
- Implementation notes list every kept date and cite with its rubric reason, so review can check the keeps, not just the deletions.
- **Success criteria:**
  - `rg -c 'Phase [0-9A-Z]' --type rust borg` prints nothing
  - Observed on main: 275 lines

##### Phase 29: strip narration, cortex (F19)
**Model:** sonnet
- Same rubric and notes. Scope: `cortex/` (tests included).
- **Success criteria:**
  - `rg -c 'Phase [0-9A-Z]' --type rust cortex` prints nothing
  - Observed on main: 222 lines

##### Phase 30: strip narration, the rest (F19)
**Model:** sonnet
- Same rubric and notes. Scope: `vault/`, `distillers/`, `oracle/`, `sb/`, `bin/`; the SQL comment at `vault/src/search/schema.rs:158`; living docs `CLAUDE.md:54`, `distillers/AGENTS.md:24,31,48`, `cortex/AGENTS.md:58`.
- **Success criteria:**
  - `rg -c 'Phase [0-9A-Z]' --type rust borg cortex vault distillers oracle sb bin` prints nothing
  - `rg -n '\bPhase [0-9A-Z]' CLAUDE.md -g 'AGENTS.md' .` prints nothing
  - Observed on main: 745 lines across all crates (455 non-test, 290 test); the living-docs probe hits `CLAUDE.md:54`, `cortex/AGENTS.md:58`, `distillers/AGENTS.md:24,31,48`

##### Phase 31: guard the archaeology (F19)
**Model:** sonnet
- `bin/source-lint` gains: `\bPhase [0-9A-Z]` in any `.rs` (tests included) or in `CLAUDE.md` / `**/AGENTS.md`; every unprefixed `docs/design/*.md` path cited in a `.rs` resolves (cross-repo cites with a repo prefix are skipped).
- **Success criteria:**
  - `bin/source-lint` exits 0
  - a planted `// Phase 3 wires this` and a planted cite of a nonexistent `docs/design/1999-01-01-x.md` each make it exit 1

## Acceptance Criteria

- [x] `otto ci` exits 0, and its output includes the `source-lint` task
  - Observed on main: exit 0, 2,775 passed, 4 ignored (2026-10-05); no `source-lint` task
  - Verified at 5774e14: exit 0, 3,059 passed, `[source-lint] source-lint: clean`
- [x] `rg -n 'try_wait|Client::new\(\)|Client::builder\(\)|param_idx|let _ = h\.await' --type rust -g '!vault/src/process.rs' -g '!vault/src/http.rs' -g '!*tests*' -g '!**/tests/**'` prints nothing
  - Observed on main: 67 lines
  - Verified at 5774e14: prints nothing (rc 1)
- [x] `bin/source-lint` exits 0 with all six checks (skip idiom, inline test modules, member lints, toolchain parity, `Phase N`, design-path resolution)
  - Observed on main: the script does not exist; the `Phase` probe alone finds 745 lines and the skip probe 10
  - Verified at 5774e14: `source-lint: clean`, rc 0; seven checks (the six above plus the freed-port idiom from the orchestrator closed-port fix)
- [x] `otto cov` runs a `unittests src/lib.rs` binary for each of vault, borg, cortex, oracle, distillers, sb; `otto ignored` reports `3 passed`
  - Observed on main: cov runs sb only (6.1% lines); no `ignored` task
  - Verified: `otto cov` at 2c10bb4 ran `unittests src/lib.rs` for all six crates (81.3% lines); its scope line is unchanged through 5774e14. `otto ignored` at 5774e14: rc 0, 3 passed (1+1+1)
- [ ] on desk after the final deploy: `sb doctor` reports no unit drift, `sb cortex graph --rebuild` leaves the fact-edge count unchanged, and `sb borg hotkey --install` binds a command that runs
  - Observed on main: installed `cortex.service` differs from its renderer (`ReadWritePaths`); `--rebuild` does not exist; the live keybinding points at a missing `obsidian-borg` binary
  - UNVERIFIED: needs the deploy and Scott's rollout steps; checked after the finalization checkpoint

## Resolved Decisions

- **2026-10-05, Scott: F1 left alone.** borg runs inside the home network. Advisory only (Addendum A).
- **2026-10-05, Scott: fix everything, no shortcuts.** Research-found instances of a flagged class are in scope.
- **2026-10-05, author: F18 narrowed** (Addendum B).
- **2026-10-05, author: links in code are not links.** Obsidian renders both fenced and inline code literally; the shared parser skips both. Changes quality and linking (which today count links in code) to match what the user sees.
- **2026-10-05, author: model-downloading tests and the rkvr test are `#[ignore]` behind `otto ignored`, which names each one.** CI does not cache the HF model; a 133 MB download per run buys a check that only changes when the embedding backend changes.
  rkvr is Scott's binary with no CI install path.
- **2026-10-05, author: F10 gets enforcement, not just a corrected comment.** The hybrid-retrieval design doc (:651) planned an assert that was never built; `otto perf` builds it in release mode, outside `ci`.
- **2026-10-05, author: one vocabulary key for oracle.** oracle reads borg.yml's `tags.canonical-path` via `BorgView` rather than a new oracle.yml key, following the 2026-10-04 "one address and one loading chain" decision.

- **2026-10-05, panel round 1 (both seats + author): signal-driven group cleanup replaces `PR_SET_PDEATHSIG`.** PDEATHSIG reaches only the immediate child.
- **2026-10-05, panel round 1 (staff, verified): `--rebuild` deletes only deterministic edge kinds, in one transaction.** The existing full rebuild's `DELETE FROM edges` would wipe fact edges.
- **2026-10-05, panel round 1 (staff, verified): resolution is indexed and keeps links.rs's title and slug fallbacks.** A path-only resolver dropped them; an unindexed one would block retrieval under the SearchIndex mutex.
- **2026-10-05, panel round 1 (staff, verified): every reqwest construction migrates in Phase 10**, so the clippy ban lands green.
- **2026-10-05, panel round 1 (staff, verified): Phase 28 split into Phases 28-30**, so every phase is one commit.
- **2026-10-05, Scott: Gate-0 fails closed on an unloadable blocklist (option A).** Raised by the architect, panel round 2. Captures are durable at the door, so a rejected capture is replayable once the file is fixed.
- **2026-10-05, panel round 2, closed (both seats AGREE): no timeout metrics.** No runtime metrics surface exists (`oracle/src/eval/metrics.rs` is eval scoring only).
- **2026-10-05, panel round 2, closed (both seats AGREE): Phase 12 external-service sites are covered by quoted WARN lines, not tests.** Serenity, teloxide, and `systemctl` are called directly with no seam; disclosed exception.
- **2026-10-05, panel round 2, closed: Gate-1 write-back (raw.rs:236).** The capture is still rejected; the architect's concern moved to Gate-0, decided by Scott above.
- **2026-10-05, panel round 1, dropped: timeout metrics (architect).** There is no metrics surface in the house pattern; the WARN logs carry the label and duration.

## Alternatives Considered

### F2: inject binaries via `PATH` override
- **Pros:** no `Command` seam.
- **Cons:** process-global; needs `serial_test`; one test's PATH leaks into parallel tests.
- **Why not chosen:** the `Command` seam is local and needs no new dev-dependency.

### F3: borg-local `http` module
- **Pros:** no new vault feature.
- **Cons:** oracle keeps its own builder; two constructors again.
- **Why not chosen:** `vault::daemon` is already the agreed home for daemon-client code.

### F16: run CI as a non-root user
- **Pros:** the chmod tests would work unmodified.
- **Cons:** rustup lives in `/root/.cargo`, apt needs root, rust-cache keys on `CARGO_HOME`.
- **Why not chosen:** ENOTDIR makes the tests root-proof with no CI change.

### F19: lint dates too
- **Why not chosen:** the house comment rule's own "Right" example has a date plus an issue link. Dates as scar-tissue evidence are legitimate.

## Technical Considerations

### Dependencies
- New direct deps: `libc` on vault (in lockfile), `reqwest` on vault behind `http` (in lockfile). No new crates in the lockfile.
- Phases run in number order. Hard dependencies (do not reorder across these): 7 -> 8; 9 -> 10; 14 -> 15 -> 16; 18 -> 19 -> 20; 0a PASS -> 23; 0d PASS -> 7; 28, 29, 30 -> 31; 26 after 9 (both touch `borg/src/lib.rs`); 0b -> 1.

### Performance
- F17 raises retrieval depth only when `limit > top_k`.
- F27 moves `fs::metadata` out of the write transaction, shortening the lock window on full reindex.

### Security
- F22 makes `reingest-failed` send the bearer like its siblings.
- F1 unchanged by decision; see Addendum A.

### Testing Strategy
- Every behavioral fix gets a test that fails on `01408d3`; each phase's implementation notes record the break-it run.
- Guards (`bin/source-lint`, clippy config, goldens, doctor drift) each get a planted-violation check.

### Rollout Plan
- Per track: PR -> `otto ci` green -> merge -> `bump` -> `otto deploy` on desk, then lappy.
- Operator steps, owner Scott, tracked here until done:
  - [ ] after Phase 16 deploy: record the fact-edge count (`sqlite3 -readonly <oracle index> "select count(*) from edges where kind='fact'"`), run `sb cortex graph --rebuild`, confirm the count is unchanged
  - [ ] after Phase 20 deploy: `sb borg daemon --install`, `sb cortex daemon --install` (writes the unit only, `cortex/src/daemon.rs:964`), then `systemctl --user daemon-reload`, `systemctl --user restart borg cortex`; confirm `sb doctor` reports no drift and `systemctl --user show -p NeedDaemonReload borg cortex` prints `no` twice
  - [ ] after Phase 24 deploy: `sb borg hotkey --install`, press the hotkey with a URL on the clipboard, confirm `sb borg log --since 5m` shows the capture
  - [ ] after Phase 25: `otto deploy` treats an extension refresh failure as non-fatal, so confirm `sb borg extension version` reports the new version; then the Phase 25 rollout check on lappy
  - [ ] after Phase 3: remove `fabric-patterns:` from `~/.config/sb/cortex.yml:96` (desk-local)

### Cross-repo blast radius
- dotfiles `HOME/.config/sb/borg.yml`: new optional keys, all defaulted; nothing required.
- dotfiles `docs/new-machine-bootstrap.md` Gap 10: closed by Phase 25.
- No other repo changes.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| Process-group kill misses a grandchild that calls `setsid` | Low | Leaked process after timeout | Phase 0d proves the common case; the helper still returns at the timeout |
| Code-skipping parse changes quality/linking counts | Med | Lint findings shift | Intended; matches what Obsidian renders. Called out in the PR |
| Graph edges stale until rebuild | High (certain) | Wrong link edges until the operator step | `sb cortex graph --rebuild` listed in rollout, with a fact-count check before and after |
| F19 strip deletes a comment's reason along with its tag | Med | Lost scar tissue | Rubric keeps the why; per-crate commits keep diffs reviewable |
| Own process group means Ctrl-C no longer reaches the child | Med | Orphaned fabric after Ctrl-C of an interactive `sb` | sb's SIGINT/SIGTERM handler kills every registered group (Phase 7); daemons covered by systemd control-group kill |
| Path-qualified link resolution differs from Obsidian on an edge | Low | Inbound count off for that note | `Resolver` table tests mirror Obsidian's documented rule; 6 duplicate stems in the live vault to test against |
| Gate-0 fails closed on a corrupt blocklist | Low | URL captures rejected until the file is fixed | Gate alert names the path; receipts are replayable with `sb borg replay` |
| Parity test fixture generation needs sentence-transformers and an HF download | Med | Phase 5 blocked | `pipx run --spec sentence-transformers` per the script's own docstring |

## Open Questions

None.

## References

- Handoff: `docs/handoff/quality-review-flaws.md`
- `docs/design/2026-10-04-ingest-queue-status.md` (config duration convention, daemon-client home)
- `docs/design/2026-05-16-hybrid-retrieval-fts5-vector-rrf.md` (F10 budgets)
- `docs/design/2026-09-19-tags-only-classification.md:702` (F13, F14 self-flags)
- `~/repos/scottidler/claude/HOME/repos/.claude/rules/comments.md` (F19 rubric)

## Addendum A: closing F1 (advisory, not planned)

State on desk: `0.0.0.0:8181` listening on LAN, Tailscale, and ~30 docker bridges; no token configured. Gated routes: `POST /ingest`, `/ingest/file`, `/note`, `GET /trace/{id}`, `/queue`. Open: `/health`, `/health/audit`.

Every first-party Rust client already sends the token when one is configured (via `vault::daemon::client_auth_token`), except `reingest-failed` (fixed regardless in Phase 9). The extension sends it once Phase 25 lets the options page save. The bookmarklet is the only client that needs CORS, and it cannot carry a token.

**Option A: token, fail closed.** Turn the warn at `borg/src/lib.rs:234-241` into a bail, like the unresolvable-token path at `:228-232`.
- Token path: `borg-auth-token.age` in keep under `secrets.env` -> the unit's `ExecStartPre` writes `borg.env` -> `server.auth-token: BORG_AUTH_TOKEN` in dotfiles borg.yml.
- Breaks: the bookmarklet; the extension until a token is entered in options.
- Lappy CLI keeps working (interactive shell has the env var).

**Option B: bind loopback + Tailscale only, drop CORS `Any`.** `server.host` per host, CORS allowlist of the extension origin.
- Closes LAN and docker-bridge exposure; Tailscale peers still unauthenticated.
- Breaks: the bookmarklet.

Recommendation if Scott takes one: A. It is the only option that closes the write path; a cross-origin `multipart/form-data` POST needs no CORS preflight, so dropping CORS alone does not stop a hostile page.

## Addendum B: rejected, broad F18

Converting every `vault::paths` helper to `Result` touches ~93 call sites, 7 of them in `Default` impls that cannot return `Result`, and contradicts the documented invariant in `CLAUDE.md:58` and `vault/AGENTS.md:41` (nothing in sb works without HOME, so panic with a clear message). The defect was inconsistency inside `Result` fns; Phase 20 fixes that.

## Addendum C: corrections to the handoff

- F12: coverage builds (`otto cov` exit 0); it measures only sb.
- F2: 6 unsafe sites, not 3.
- F5: 7 regexes, not 6.
- F13: `extract_wisdom` is also uncalled.
- F20: `serve_init` itself does no client work; `let _ = command;` silences nothing (`command` is used at `borg/src/service.rs:418`); 2 inline test modules, not 1.
- F19: measured 455 lines across all crates (borg 161, distillers 75, vault 78, cortex 123, oracle 7, sb 7, bin 4).

## Addendum D: Phase 0 results

Run 2026-10-05 on `desk` against HEAD 01408d3, in a throwaway worktree (`git worktree add ~/.cache/sb-0a HEAD`, removed with `git worktree remove --force` afterward). Nothing was committed.

Hardware: `Genuine Intel(R) CPU @ 3.10GHz`, 32 logical CPUs (`lscpu`).

### 0a: `test-util` stays off in a release build: PASS

Spike diff (worktree only): `test-util = []` under `[features]` in `vault/Cargo.toml`, and `vault = { path = ".", features = ["test-util"] }` under `[dev-dependencies]`.

```
cargo build --release --workspace                                          # Finished, exit 0
cargo tree --workspace -e normal,features -i vault@0.15.13 | rg test-util  # prints nothing, rg exit 1
cargo tree --workspace -e normal,features,dev -i vault@0.15.13 | rg test-util
-> ├── vault feature "test-util"                                           # positive control: dev edges do see it
```

Gotchas for Phase 23:
- Write the probe as `-i vault@<version>`, not `-i vault`. Run from the repo root, the bare `vault` argument was rewritten to the absolute path of the `vault/` directory by the session environment and cargo rejected it (`looks like a file path`), which makes `| rg test-util` print nothing for the wrong reason. The positive control above is what proves the probe can see the feature.
- A self dev-dependency does not leak the feature into the normal graph.
- `cargo` runs outside the Bash sandbox in this setup, so a worktree under the sandbox `$TMPDIR` (tmpfs, 16G, shared and nearly full) ran out of space (`No space left on device`). Use a disk-backed path for a workspace build.

### 0b: `search_vector` at 21K rows x 384 dims: measured

Scratch harness `vault/tests/spike_0b.rs` in the worktree (not committed): in-memory `SearchIndex`, 21,000 notes with one `Summary` row each, deterministic unit-norm vectors, `set_active_embedding("spike-v1", 384)`, one warmup call, then 20 timed `search_vector(q, 10, None, false, None, None)` calls. Run as `cargo test [--release] -p vault@0.15.13 --features vec --test spike_0b -- --nocapture`. p50 is the 10th and p99 the 20th of 20 sorted samples.

| Profile | min | p50 | p99 (max of 20) |
|---|---|---|---|
| release | 43.49 ms | 44.37 ms | 46.91 ms |
| debug | 287.23 ms | 292.31 ms | 314.51 ms |

Release samples (ms): 43.49 43.86 44.1 44.23 44.23 44.23 44.3 44.33 44.35 44.37 44.43 44.48 44.51 44.67 44.8 45.24 45.48 45.87 46.49 46.91.

Finding for Phase 1: release p50 is 44.37 ms, more than twice the "well under 20 ms" design target the `vector.rs` doc comment claims, on a 3.10 GHz 32-CPU host. The comment fix in Phase 1 must not keep "well under 20 ms" as a measured fact; it is a design target that the current brute-force scan misses. Regression ceiling for `perf_search_vector_21k` = 10x release p50 = 450 ms (44.37 x 10 = 443.7, rounded up), written into the test with this hardware line.

### 0c: ntfy keepalive interval: measured on ntfy.sh, not on a live borg subscription

The live `~/.config/sb/borg.yml` (dotfiles `HOME/.config/sb/borg.yml`) has no `ntfy:` section, so borg on `desk` has no live ntfy subscription to observe. `borg/src/ntfy.rs` subscribes with `GET {server}/{topic}/json` (optional `Authorization: Bearer`, optional `?since=<id>` on reconnect) and the example config's default server is `https://ntfy.sh`. The measurement used that same URL shape against `https://ntfy.sh` with a throwaway random topic and no token (sandbox blocked the host once; re-ran with `ntfy.sh` allowed).

`curl -sSN --max-time 305 https://ntfy.sh/<throwaway-topic>/json`, each line stamped on receipt: one `open` event at t=0.47 s, then `keepalive` events at t = 45.43, 90.43, 135.43, 180.43, 225.43, 270.44 s after the request began. Interval: 45.0 s (6 keepalives; 5 keepalive-to-keepalive gaps of 45.000 to 45.003 s, first keepalive 44.96 s after `open`), 305 s total. This is ntfy's server default; a self-hosted server can set its own `keepalive-interval`, so the value is a property of the server, not of borg.

`ntfy.read-timeout` = 3 x 45 s = **135s** (default for the new key). Caveat: a self-hosted ntfy server with a longer `keepalive-interval` would need the config key raised; the key exists for that reason.

### 0d: process-group kill and SIGINT handler: PASS

Scratch program (`rustc -O`, std plus `extern "C"` `killpg`/`kill`/`signal`, not committed): spawns `sh -c 'sleep 30 & head -c 1048576 /dev/zero | tr "\0" a'` with `CommandExt::process_group(0)`, stdout and stderr piped, one drain thread per pipe (`read_to_end`).

Setup observed at +0.5 s: group members `sh <defunct>` and `sleep 30`; drain thread still blocked. That is the F2 hazard: `sh` has already exited, but the orphaned `sleep 30` holds the pipe write end open, so a drain that waits for EOF would block for 30 s.

1. Kill the group at ~2.0 s with `killpg(pgid, SIGKILL)`: both drain threads returned immediately (stdout = 1,048,576 bytes, stderr = 0), `sh` reaped with status 0, `pgrep -g <pgid>` after the kill printed nothing. PASS.
2. The program sends itself `SIGINT` (`kill(getpid(), SIGINT)`) with a handler that calls `killpg(SIGKILL)` on every registered pgid (static `AtomicI32` array, async-signal-safe): drain threads returned, `pgrep -g <pgid>` empty. PASS. Process survived the signal because the handler does not exit; Phase 7's handler decides its own exit.

Leftover check, run unsandboxed (`pgrep -af 'sleep 30'`): two `sleep 30` processes appeared at different times, both parented by `bash /home/saidler/bin/sweep-repos --ensure-free 40 --watch 30` (a pre-existing loop on the host, its own process group, started seconds before each check). Neither is from the spike. No spike process remained.

Limit proven: the common case. A grandchild that calls `setsid` leaves the group and is not killed (already in the risk table).

## Addendum E: receipts test isolation, fixture-owned resources (Scott, 2026-10-05)

Context: an out-of-plan fix during execution. Two borg tests (`test_ingest_endpoint`, `write_route_accepts_correct_token`) reached `receipts::open_default()` without `harvest::TEST_XDG_LOCK`, collided with a session test's tempdir DB ("database is locked", reproduced 2/300 full-suite runs), and with `XDG_DATA_HOME` unset opened the live `~/.local/share/sb/borg/receipts.db`. The shipped fix (`a9c617d`) keeps the env-var design: wrap the two tests in `with_xdg_data_home`; `receipts::set_wal_mode` retries `PRAGMA journal_mode=WAL` on SQLITE_BUSY (two connections converting the same fresh file to WAL get an immediate "database is locked" regardless of busy timeout; rusqlite already sets a 5 s busy timeout, so the PRAGMA-order change first proposed was a no-op and was not made); and a `#[cfg(test)]` guard that panics on any default-path receipts open outside an XDG sandbox.

Scott's comment: "can we not have fixtures that setup their own resources in unique spaces rather than reuse/share a common resource? is there some performance reason for not just spinning up n-number of resources necessary and then tearing down afterwards? this is the value of having a testing fixture is it not?"

Answer recorded:
- No performance reason: a tempdir plus a fresh SQLite file costs about a millisecond per test.
- The sharing is forced by production code, not by the tests: borg resolves the receipts path from the process-global `XDG_DATA_HOME` at call time (`receipts::open_default` -> `vault::receipts::receipts_db_path` -> `vault::paths::xdg_data_dir`). Per-test isolation therefore means mutating a global env var, which needs `TEST_XDG_LOCK`, which every test must remember.
- The fixture-shaped design: inject the receipts location (or a `Receipts` handle) through `AppState` / `Config`, resolved from XDG once at startup. Each test creates its own tempdir DB and passes it in: no env mutation, no lock, full parallelism. Blast radius about 10 call sites: intake, routes `/trace`, watchdog, triage, dedupe, backfill, harvest, `TraceLeaseGuard`, the transport doors.

Decision: Scott kept the shipped lock-plus-guard fix for this plan. Injection is the road not taken here, recorded as the preferred design for a follow-up doc.
