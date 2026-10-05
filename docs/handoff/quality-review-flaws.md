# Handoff: second-brain quality review flaws

Branch: main

## Next action

Close F1 (borg's write API is open by default). Then F2 (three subprocess loops that deadlock on large output). Both are the same kind of fix: make the safe path the default, then add a test that fails if someone reverts it.

## Read first

1. This file, the flaw list below.
2. `borg/src/config.rs:1178-1186`, `borg/src/routes.rs:50-60`, `borg/src/lib.rs:104-110,225-242` (F1).
3. `vault/src/fabric.rs:200-235` (the correct drain-while-waiting primitive), then `borg/src/fabric.rs:10-34,73-95`, `borg/src/extraction.rs:20-45`, `borg/src/ocr.rs:95-116` (F2).
4. `docs/design/2026-10-04-ingest-queue-status.md:290,363` (where F1 was self-flagged as "Low").
5. `docs/design/2026-09-19-tags-only-classification.md:702` (F13, F14).

## State at time of writing

- Branch `main`, HEAD `01408d3` ("Bump version to v0.15.13"). No code changed by this work; this file is the only addition, uncommitted.
- No PR open.
- `otto ci` on `01408d3`: exit 0, 2,775 tests passed, 4 ignored (run 2026-10-05).
- Origin: a side-by-side quality review against `tatari-tv/rerun` (a TypeScript service in the same problem space). second-brain scored about 7.5/10 vs rerun's 6.5. What second-brain does better: CI runs the full suite with `clippy -D warnings` and a 1,500-line file cap, `vault` is a single shared type crate, vector search is an exact scan with SQL pre-filtering, MCP schemas are guarded by a router-parity test. The flaws below are what keeps it from an 8.
- "Verified" means checked by hand against `01408d3` in this session. "Reported" means found by a review subagent and not re-checked by hand: confirm with the probe before fixing.

## Flaws

Ordered by how much they matter. Each one has a probe you can rerun.

### F1. borg HTTP server is open by default (security) [verified]

- `ServerConfig::default()` binds `0.0.0.0:8181` with `auth_token: None` (`borg/src/config.rs:1181-1183`).
- `require_auth` returns `next.run(request)` when no token is set (`borg/src/routes.rs:55-57`), so `/ingest`, `/ingest/file`, `/note` accept anyone.
- CORS is `allow_origin(Any)` for GET and POST with `Authorization` allowed (`borg/src/lib.rs:106-109`): any web page open in a browser on the network can drive it, and can read `/queue` and `/trace` cross-origin.
- A non-loopback bind with no token only `log::warn!`s (`borg/src/lib.rs:234-241`).
- Live config is exposed: `~/.config/sb/borg.yml` has `host: "0.0.0.0"` and no `auth-token`.
- `borg/src/tests.rs` tests 401s when a token IS set, but nothing asserts the default is closed.
- Probe: `rg -n 'host|auth-token' ~/.config/sb/borg.yml; sed -n 1178,1186p borg/src/config.rs; sed -n 50,60p borg/src/routes.rs`
- Fix direction: refuse to start on a non-loopback bind without a token (fail closed, like the unresolvable-token path at `lib.rs:228-231` already does), or default `host` to `127.0.0.1`. Restrict CORS to known origins. Add a test that the default config refuses an unauthenticated write. Then set a token in the live config.

### F2. Three subprocess timeout loops deadlock on output above 64 KiB (correctness) [verified]

- Pattern: spawn with `stdout(piped())`, poll `try_wait()` every 100 ms without reading the pipe, read output only after exit. A child that writes more than the pipe buffer blocks on write, never exits, gets killed at the timeout.
- `borg/src/fabric.rs:77-88` (`fetch_transcript`): long YouTube transcripts. On timeout returns `Ok(String::new())` with no log at this layer, then the caller falls back to yt-dlp subtitles after burning the full timeout.
- `borg/src/extraction.rs:20-45` (markitdown): large documents time out and bail.
- `borg/src/ocr.rs:95-116` (tesseract): timeout returns `Ok(String::new())`, fail-open.
- The deadlock-safe version already exists: `vault::fabric::wait_with_timeout` (`vault/src/fabric.rs:200-235`). `borg/src/fabric.rs:10-13` even names it. Four copies of the loop, three wrong.
- No test writes more than the pipe buffer; `pipeline/timeouts.rs` tests kill-on-timeout with `sleep`, which writes nothing.
- Probe: `sed -n 73,95p borg/src/fabric.rs; sed -n 20,45p borg/src/extraction.rs; sed -n 95,116p borg/src/ocr.rs`
- Fix direction: route all three through one drain-while-waiting helper (the vault one, generalized if needed). Add a test with a child that writes ~1 MiB and exits fast; it must succeed well inside the timeout.

### F3. HTTP clients with no timeout, and silent timeout loss (robustness) [verified]

- Raw `reqwest::Client::new()` (no timeout) on daemon calls: `borg/src/lib.rs:828,902`, `borg/src/replay.rs:259,291`, `borg/src/migrate.rs:405`, `borg/src/ntfy.rs:103`, `borg/src/stages/fetcher.rs:100`.
- Fallback-to-`Client::new()` when the builder fails silently drops the configured 30 s timeout: `borg/src/github.rs:294,309`, `borg/src/stages/fetcher.rs:217`.
- 19 ad-hoc client constructions, no shared builder [reported].
- Probe: `rg -n 'Client::new\(\)' borg/src --type rust -g '!*tests*'`
- Fix direction: one `http_client(timeout)` constructor in a shared spot; a builder failure is an error, not a downgrade.

### F4. Error swallowing that loses state (correctness) [verified]

- `cortex/src/summarize.rs:166`: `let _ = save_checkpoint(..)` loses resume state silently.
- `cortex/src/summarize.rs:189-191`: `for h in handles { let _ = h.await; }` drops task panics, so the final counts are wrong.
- `cortex/src/naming.rs:257-259`: rename link-rewrite does `Err(_) => continue` on an unreadable file with no log; a rename can leave dangling links.
- `borg/src/pipeline/handlers.rs:354`: `let _ = slides::write_manifest(..)` drops the error.
- Probe: `sed -n 164,192p cortex/src/summarize.rs; sed -n 255,260p cortex/src/naming.rs; sed -n 352,356p borg/src/pipeline/handlers.rs`
- Fix direction: log at WARN with the path at minimum; propagate where the caller's result depends on it (checkpoint, task join).

### F5. Wikilink regex defined 6 times in 3 variants (drift) [verified]

- Modules disagree on what a link is:
  - `vault/src/search.rs:24` excludes `#` (heading anchors split off).
  - `cortex/src/unlink.rs:30` excludes `#` and `^` and `[`.
  - `cortex/src/quality.rs:13`, `cortex/src/links.rs:11`, `cortex/src/linking.rs:84`, `borg/src/dedupe.rs:69` use a third form that treats `[[Note#Heading]]` as target `Note#Heading`.
- Consequence: link counts, backlinks, unlink, and dedupe can disagree on the same note.
- Probe: `rg -n -F '\[\[' --type rust -g '!*tests*' | rg Regex`
- Fix direction: one regex plus a `parse_wikilink` in `vault`, used everywhere. Test it against `[[a]]`, `[[a|b]]`, `[[a#h]]`, `[[a#^block]]`, `[[a#h|b]]`.

### F6. Hand-rolled dynamic SQL builder repeated 7 times (duplication) [verified]

- Each copy ends with `let _ = param_idx;` to silence the unused counter: `vault/src/search/vector.rs:240`, `vault/src/search/query.rs:90,182`, `vault/src/search/stats.rs:310,424,474,620`.
- Probe: `rg -n 'let _ = param_idx' vault/src`
- Fix direction: one small filter-clause builder that returns `(sql, params)`.

### F7. Two systemd unit generators kept in sync by hand (duplication) [verified]

- `cortex/src/daemon.rs:930-1027` and `borg/src/service.rs:227-316`. The comment at `cortex/src/daemon.rs:884` says "Matches borg's unit".
- Probe: `rg -n "Matches borg" cortex/src`
- Fix direction: one generator in `vault` (or a small shared module) parameterized by binary and args.

### F8. Two backoff implementations (duplication) [verified]

- `borg/src/backoff.rs` plus a hand-rolled `retry_backoff` at `borg/src/transcription.rs:231`.
- Probe: `rg -n 'fn retry_backoff' borg/src`
- Fix direction: fold transcription onto `backoff.rs` (it needs Retry-After support; add it there).

### F9. Lint policy comment contradicts the code (lying doc) [verified]

- `Cargo.toml` workspace comment says `clippy::unwrap_used` "stays a per-crate `#![deny]`". Only `borg`, `distillers`, `sb`, `vault` have it. `cortex` and `oracle` do not.
- Production `.unwrap()` count in cortex/oracle is still zero today [reported], but nothing stops one landing.
- Probe: `rg -l 'deny\(clippy::unwrap_used\)' */src/lib.rs */src/main.rs`
- Fix direction: add `#![deny(clippy::unwrap_used)]` to `cortex/src/lib.rs` and `oracle/src/lib.rs`, with `#[allow]` on test modules like the others.

### F10. Benchmark "enforces" a budget it does not assert (lying doc) [verified]

- `vault/src/search/vector.rs:198-201`: "under 20 ms ... Phase A7's benchmark enforces the budget."
- `vault/benches/hybrid.rs:16-20`: "The bench does not assert that budget" and the budget it cites is 200 ms, not 20.
- Probe: `sed -n 196,202p vault/src/search/vector.rs; sed -n 14,21p vault/benches/hybrid.rs`
- Fix direction: correct the comment to what is true (no enforced budget, design target 200 ms p50), or add an asserting test.

### F11. Stale "Phase 2" doc comment (lying doc) [verified]

- `borg/src/intake.rs:74-75`: "Replaces `record_intake` ... once the call sites are switched in Phase 2." `record_intake` no longer exists anywhere.
- Probe: `sed -n 72,76p borg/src/intake.rs; rg -n 'fn record_intake' --type rust`
- Fix direction: delete the sentence.

### F12. `cov` task is broken (CI hygiene) [verified]

- `.otto.yml:86` runs `cargo llvm-cov --all-features`. The same file's comment (`.otto.yml:58-61`) says `--all-features` trips the `vec-candle` / `vec-fastembed` `compile_error!` guard. Coverage cannot build; nobody noticed because `cov` is not part of `ci`.
- Probe: `sed -n 55,62p .otto.yml; sed -n 85,89p .otto.yml; otto cov; echo $?`
- Fix direction: use `--features vec`, same as `test`.

### F13. Doctor requires a fabric pattern nothing calls (self-flagged, open) [verified]

- `sb/src/cli/checks.rs:374` requires `create_tags`. Flagged in `docs/design/2026-09-19-tags-only-classification.md:702`, never fixed.
- Probe: `sed -n 372,376p sb/src/cli/checks.rs; rg -n '"create_tags"' --type rust`
- Fix direction: drop it from the required list.

### F14. oracle reads the tag vocabulary from the default path, fails open (self-flagged, open) [verified + reported]

- `oracle/src/server.rs:1172-1184` loads `vault::paths::canonical_tags()` (default path only); cortex uses the configured `canonical_path` and fails closed (`cortex/src/sweep.rs:177`) [reported].
- On load error it warns and returns an empty Vec, so `tag_brief` reports real canonical tags as unknown and `schema_info` returns no tags. The doc comment says this is deliberate for hermetic tests; the cost is a silent wrong answer in production.
- Flagged in `docs/design/2026-09-19-tags-only-classification.md:702`.
- Probe: `sed -n 1170,1185p oracle/src/server.rs; rg -n 'canonical_path' oracle/src cortex/src`
- Fix direction: read the configured path; surface the load failure in the tool response, not just the log.

### F15. Test-only code shipped as `pub` to dodge `deny(dead_code)` (slop) [verified]

- `distillers/src/fabric.rs:118` `pub struct FakeFabric`, `borg/src/stages/artifact.rs:405` `pub struct MemArtifactStore`, `vault/src/search/vector.rs:847-897` `insert_test_note_row` and `set_test_*`.
- `cortex/src/association.rs:64` `group_by_slug`: no production caller, reached only by tests. Flagged "Deferred, NOT fixed" in `docs/design/2026-08-15-harvest-note-identity-trace-keyed-replace-implementation-notes.md:756`.
- Probe: `rg -n 'pub struct (FakeFabric|MemArtifactStore)|pub fn (insert_test_note_row|set_test_)' distillers borg vault; rg -n 'group_by_slug' --type rust -g '!*tests*'`
- Fix direction: gate behind `#[cfg(any(test, feature = "test-util"))]`; delete `group_by_slug` and move its tests to the replacement.

### F16. Root-in-CI tests that skip themselves and pass (false green) [verified]

- `cortex/src/proposals/tests.rs:667` and `cortex/src/sweep/tests.rs:849` [reported for the second]: when chmod has no effect (CI runs as root in `debian:bookworm`), they `eprintln!("skipping...")` and `return`, so they pass without testing.
- Probe: `sed -n 665,670p cortex/src/proposals/tests.rs; rg -n 'skipping:' --type rust`
- Fix direction: run the CI job as a non-root user, or mark these `#[ignore]` with a reason so the skip is visible.

### F17. oracle `limit` above 50 silently truncates (API contract) [reported]

- Per-method `top_k` defaults to 50 (`oracle/src/config.rs:312-314`); `limit` is not clamped or documented in the tool schema (`oracle/src/tools.rs:99`), so `limit: 100` returns at most 50.
- Probe: `sed -n 310,314p oracle/src/config.rs; sed -n 95,103p oracle/src/tools.rs`
- Fix direction: clamp `limit` to `top_k` and say so in the schema, or raise `top_k` to `max(limit, top_k)` per call.

### F18. Panics on missing `HOME` (robustness) [reported]

- XDG `.expect`s: `vault/src/paths.rs:101,279-408`, `borg/src/blocklist.rs:130`, `borg/src/harvest/timer.rs:123,156`, and inside `Result`-returning functions at `cortex/src/daemon.rs:886,955,991,1027`.
- Probe: `rg -n 'expect\(' vault/src/paths.rs cortex/src/daemon.rs borg/src/blocklist.rs borg/src/harvest/timer.rs`
- Fix direction: return an error from the path helpers; the callers already return `Result`.

### F19. Process archaeology in comments (slop) [reported counts]

- About 450 `Phase N` references across borg/distillers/vault (297 in non-test code), 122 more in cortex production code; 533 inline dates and 59 `docs/design/...` paths in `.rs` files. Examples: `distillers/src/idea.rs:7`, `cortex/src/association.rs:17`, `vault/src/search/vector.rs:281`.
- Comments explaining WHY are fine. "Phase 5 (this phase) wires it all" is session narration that rots (F10, F11 are two that already rotted).
- Probe: `rg -c 'Phase [0-9A-Z]' --type rust -g '!*tests*' | awk -F: '{s+=$2} END {print s}'`
- Fix direction: mechanical pass that strips phase/version/date narration and keeps the reason. Consider a lint in `otto lint` that fails on `Phase [0-9]` in non-test `.rs`.

### F20. Smaller items [reported]

- `borg/src/lib.rs:210-485`: 275-line `serve_init` mixes server setup and CLI client commands; nearest thing to a god function.
- `borg/src/service.rs:372,387`: `let _ = (host, port);` / `let _ = command;` silence unused params instead of removing them.
- `vault/src/search/index.rs:27-37`: full reindex calls `fs::metadata` per note while holding `BEGIN IMMEDIATE`.
- One inline `mod tests {}` survived the extraction pass: `vault/src/tombstone.rs`.
- Several files sit just under the 1,500-line cap: `sb/src/cli/borg.rs` 1440, `sb/src/cli/checks.rs` 1333, `cortex/src/embed.rs` 1278.
- `release.yml` pins Rust 1.96, CI uses 1.98 (disclosed in-file).

## Dropped during verification

- "`tower` is only used in tests": false, `borg/src/lib.rs` uses it.
- "oracle vocabulary load fails silently": it warns (`oracle/src/server.rs:1180`). Still fails open (F14).

## Session-scoped notes

- Nothing running. The rerun review's throwaway Postgres container was removed.
- `otto ci` result above is from 2026-10-05; rerun it after any change.

## Suggested skills

- `create-design-doc` for F1 (it changes default runtime behavior and the live config); targeted fixes for F2-F18.
- `how-to-execute-a-plan` if F1-F5 are batched into one phased doc.
- `otto` for `otto ci` after each fix.
- `shipit` once a batch is green.
