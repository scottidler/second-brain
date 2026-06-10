# Design Document: Code Review Remediation

**Author:** Scott Idler
**Date:** 2026-06-09
**Status:** In Review
**Review Passes Completed:** 5/5

## Summary

A workspace-wide code review (eight parallel reviewers: one per crate, two for borg, one cross-cutting; ~160 deduplicated findings, several confirmed empirically) found 7 critical bugs, ~40 majors, and a long tail of minors/nits. This doc converts every finding into a phased fix plan. Every named finding gets coverage here — nothing is deferred or "tracked separately."

## Problem Statement

### Background

The 2026-06-09 review examined all six crates (vault, borg, cortex, oracle, distillers, sb), the config/templates, bin/ scripts, and browser clients, validated against the real vault at `~/repos/scottidler/obsidian` and the live daemon host. Several findings were confirmed by running probe code (the watcher panic, the YAML escaping corruption, the fence-strip truncation, the slug punctuation leak).

### Problem

Five systemic defect classes, plus per-crate bugs:

1. **Silent death:** the vault watcher panics after its first batch; transport task failures are swallowed; background-task panics land in never-awaited JoinHandles. The system degrades without any operator signal — the "worked for weeks then broke" class.
2. **Fail-open defaults:** Telegram's empty allowlist accepts every chat; `is_local_host` returns true on error (defeating the Signal single-machine pin); oracle silently loads `./oracle.yml` from CWD.
3. **Non-atomic vault mutation on a Syncthing'd vault:** ~20 cortex sites and 5 borg publish sites use in-place `fs::write`; a torn write replicates to every machine.
4. **Receipts taxonomy decay off the happy path:** stage-0 rejections and transport download failures rot in `received` and get mislabeled `crashed`; replay accounting is structurally dead (always `Queued` → 0/0 reports).
5. **Duplication tax:** `strip_fences` ×6 (with a live truncation bug in all 6), five frontmatter splitters, three `update_wikilinks_for_moves`, two `truncate_input`, positional ledger parsing beside the named-column parser built to replace it, deps not hoisted to `[workspace.dependencies]` with skew already present.

### Goals

- Fix all 7 criticals with regression tests in the same change.
- Eliminate the five systemic classes, not just their instances.
- Bring the older half of the codebase up to the user's own rules (CLI conventions, test placement, entry-DEBUG logging, schema-is-law).
- Close every test gap the review identified.
- Leave docs/contracts (AGENTS.md, MCP tool descriptions, CLAUDE.md policies) telling the truth.

### Non-Goals

- New features, new retrieval methods, new transports.
- Performance work beyond the specific findings (no ANN/SIMD — desk's CPU constraint stands).
- Vault data migrations beyond what schema-is-law requires (existing `status: processed` notes are tolerated, not rewritten).
- Raising the 1500-line bloat gate (files get decomposed instead).

## Proposed Solution

### Overview

Fifteen phases, ordered by severity and dependency: criticals first, then runtime hygiene, receipts integrity, atomicity, fail-closed defaults, schema-is-law, error-handling integrity, bloat decomposition (before refactors touch those files), cross-crate dedup, per-crate remainders (oracle, cortex, vault, sb), docs/rule sweeps, and test-gap closure. Phases ship back-to-back — no burn-in, no soak gates. Every fix lands with its regression test in the same phase.

### Architecture

No new components. The structural changes are consolidations into `vault` (the shared foundation):

- `vault::note::write_atomic` (tmp + fsync + rename + parent fsync) becomes THE note-write primitive; all cortex/borg mutation sites route through it.
- `vault::frontmatter::split_raw(&str) -> Option<(&str, &str)>` replaces the five ad-hoc fence splitters.
- `vault::fabric` gains drained-pipe subprocess handling and exports `wait_with_timeout`; a typed `FabricError::Timeout` replaces string matching.
- A `distillers::parse` module owns `strip_fences`, `approx_tokens`, the `PatternYaml`/`PatternClaim`/`PatternLink` structs, and the transcript chunker (shared by video/voicenote).
- A `vault::search` query helper maps `QueryReturnedNoRows → None` and propagates real SQLite errors.
- Borg exposes typed `probe_telegram()`/`probe_signal()` so sb's doctor drops its duplicate transport deps.

### Data Model

One schema change: the receipts DB gains a `degraded INTEGER NOT NULL DEFAULT 0` column on the success path (added via the existing receipts migration mechanism that runs on open), marking notes published from a distill fallback. `sb borg log --degraded` filters on it. This formally retires the dead "hard distill failures halt and route to DLQ" policy: degraded publishes are the documented behavior (2026-05-18 data-loss rationale), now queryable instead of invisible.

`vault::schema::NoteType` gains `Digest` and `Review` variants (76 real notes currently warn-spam and index with empty type). No new `Status` variant: `intel::process_new_notes` (the only writer of `status: processed`, dead code — zero callers) is deleted; the `"processed"` match arms remain for legacy notes.

### API Design

No public MCP/HTTP surface changes except:

- `ingest_history` gains `limit: Option<u32>` (default 50) — currently it can return the entire multi-thousand-row ledger in one MCP response.
- `find_links.direction` and `quality_report.quality` become schema enums so typos fail deserialization instead of silently returning empty results.
- `/ingest` multipart `tags` accepts repeated fields instead of comma-splitting (CLI-rule parity at the wire).
- `sb borg ingest/note --tags` drops `value_delimiter = ','` for `num_args = 0..`.
- New `GET /trace/{trace_id}` endpoint (auth-gated like the write routes) returning the receipts row state (`received`/`succeeded`/`failed`/`crashed` + stage + note path). Replay/reingest poll it for terminal state — they cannot read the receipts DB directly because client hosts (laptop) don't own one; the DB is per-host on the daemon.

### Implementation Plan

#### Phase 1: Critical correctness fixes
**Model:** opus

- [ ] `vault/src/watcher.rs:162` — `reset(Instant::now() + Duration::MAX)` panics (tokio `Instant + Duration` overflow, empirically confirmed); the debounce task dies after the FIRST emitted batch and every watcher consumer (oracle live reindex, cortex daemon) gets exactly one change event per process lifetime. Fix: reset to a far-future instant (`now + Duration::from_secs(86400 * 365)`); add a test that receives TWO batches.
- [ ] `cortex/src/tags.rs:193` — `replace_tags_in_frontmatter` only consumes indented `-` bullets; column-0 block tag lists (present in real vault notes) get `tags: [new]` inserted with the old bullets orphaned → invalid frontmatter; daemon auto-applies sweep. Fix: delete the duplicate continuation logic and route tag rewrites through `scope`'s continuation-aware helpers (`is_continuation` accepts `- ` at column 0). Same class: `cortex/src/migrate.rs:338-343` field drops — reuse `scope::remove_entry`. Tests on column-0 lists for both.
- [ ] `borg/src/pipeline/handlers.rs` — only `process_url` has a hard timeout; `process_image`/`process_audio`/`process_document_file` and `text.rs` handlers await unbounded. `ocr.rs:106` builds the Anthropic vision `reqwest::Client` with NO timeout. A wedged trace holds its GENERAL permit forever and the watchdog skips it (active-trace exclusion) → 8 wedged traces = silent total ingest deadlock. Fix: wrap every handler in `tokio::time::timeout(hard_timeout, ...)` like `process_url`; give the vision client a timeout.
- [ ] `borg/src/lib.rs:146-161` — `ServerHandle::wait` logs a failed daemon task and keeps waiting; `signal.rs:880-887` bails on `Deauthorized` claiming systemd will surface it — it never does. Fix: any task resolving to `Err` aborts the JoinSet and returns the error, so `Restart=always` + `sb doctor` see it.
- [ ] `borg/src/markdown.rs:263-265` — `escape_yaml_string` only escapes `"`; a trailing `\` or embedded newline in any LLM-derived frontmatter value (`cortex-repo-install`, `title`, `creator`, ...) corrupts the entire frontmatter block (empirically confirmed). Fix: serialize scalars via `serde_yaml::to_string` (trimmed); property-style tests for backslash/newline/colon/quote inputs.
- [ ] `oracle/src/config.rs:7` — derived `Default` yields `inbound_recompute_interval_secs = 0`; `tokio::time::interval(0)` panics in a never-awaited JoinHandle on any host without `oracle.yml`. Fix: manual `impl Default` using the serde default fns; test asserting `Config::default() == serde_yaml::from_str("{}")`.
- [ ] `borg/src/telegram.rs:228` — empty `allowed-chat-ids` (the serde default) is fail-open: every chat accepted. Fix: empty allowlist = deny-all, mirroring Signal; extract pure `fn chat_allowed(allowed: &[i64], chat_id: i64) -> bool` with tests; `sb doctor` warns when telegram is enabled with an empty allowlist.

#### Phase 2: Runtime hygiene — timeouts, blocking calls, subprocess I/O
**Model:** opus

- [ ] `vault/src/fabric.rs:36-90` — `run_pattern` never drains stdout during the poll loop; output past the ~64KB pipe buffer deadlocks the child until the timeout kills it, misreported as `fabric-timeout` (found independently by three reviewers). The pre-loop `stdin.write_all` also blocks unbounded (truncation is char-capped, not byte-capped). Fix: reader thread per pipe, stdin written from a thread, the timeout covering the whole lifecycle.
- [ ] `borg/src/youtube.rs:269-283, 426-429` — `extract_audio` (yt-dlp full download) and `extract_frames` (ffmpeg) are blocking `std::process::Command::output()` calls with no timeout inside async fns; even the URL hard-timeout can't interrupt a blocked thread. Fix: `tokio::process` + `timeout` + `kill_on_drop` (the file already does this for `fetch_metadata`/`fetch_subtitles_raw`).
- [ ] Blocking subprocess waits on the runtime: `borg/src/fabric.rs:101-141` (`fetch_article`), `extraction::extract_markdown` call in `handlers.rs:972` — wrap in `spawn_blocking` (only `fetch_transcript` is wrapped today).
- [ ] `oracle/src/server.rs:777` — the global `SearchIndex` mutex is held across `fabric_transform` (60s blocking subprocess), `embed_query` (fastembed inference), and rerank; one flaky query freezes every MCP tool call and the watcher task. Fix: run transform before taking the lock (it doesn't need the DB); wrap lock-holding retrieval in `block_in_place`.
- [ ] `cortex/src/daemon.rs:340` — only `ctrl_c()` is handled; systemd stops with SIGTERM, killing the daemon mid-write. Fix: add a `SignalKind::terminate()` select arm.
- [ ] `cortex/src/embed.rs:179-211, 304-327` — a persistently failing `embed_batch` makes a zero-progress batch that re-selects the identical stale set: tight infinite retry loop. Fix: break on any zero-progress batch (`embedded == 0`), not only the all-skipped case.
- [ ] `cortex/src/embed.rs:718` — `daemon_cadence(_config)` ignores its parameter; cadence is hardcoded despite CLAUDE.md documenting it as configurable. Fix: add `embed.cadence-secs` to `EmbedConfig` and read it.
- [ ] `cortex/src/daemon.rs:142` — embedding-model load failure crash-loops the entire governance daemon. Fix: degrade to "embed tick disabled" with an ERROR; every non-embed action still runs.
- [ ] `borg/src/discord.rs:213-227, 271-283` — the pipeline runs inline in the serenity event handler, coupling acks to the gateway client lifetime. Fix: `tokio::spawn` + detach, mirroring telegram.
- [ ] Backoff resets fire before the work loop in all three transports (`telegram.rs:177-187`, `discord.rs:311`, `signal.rs:786`), so immediate post-handshake failures hot-loop at ~1s forever. Fix: reset only after sustained healthy operation (first successfully dispatched message).
- [ ] `borg/src/signal.rs:431-446` — a tripped rate gate sends one outbound alert per dropped envelope — unbounded alert spam in exactly the flood it guards against. Fix: latch (alert once on trip, `log::error!` thereafter).
- [ ] `borg/src/extraction.rs:7` — hardcoded `MARKITDOWN_TIMEOUT_SECS = 30` ignores the existing `pipeline.markitdown_timeout_secs` config knob (default 60). Fix: thread the config value through.
- [ ] `borg/src/ocr.rs:23` — hardcoded `/usr/bin/tesseract`; use the bare name like every other tool.
- [ ] `borg/src/pipeline/handlers.rs:239-241` — `/tmp/borg-youtube-frames/<video_id>` (≤720p mp4 + all frames) is never deleted. Fix: remove the work dir after `publish_slides` copies the JPEGs out.

#### Phase 3: Receipts & durable-capture integrity
**Model:** opus

- [ ] `borg/src/pipeline.rs:96-106` — stage-0 rejection returns BEFORE the terminal-write chokepoint (line 150); the row rots in `received` and the watchdog mislabels it `crashed` ~31 min later with the wrong stage. Fix: write the terminal receipt before the early return; add a receipts-state test for stage-0 rejection.
- [ ] `borg/src/telegram.rs:279-525` (photo/voice/audio/document download failures + unsupported types) and `borg/src/discord.rs:167-247` — every early-return after intake leaves the row stuck in `received`. Fix: `record_failure_at_door(.., FetchFailed/IntakeRejected, ..)` on every branch (signal.rs:534-539 is the model); test that a failed download lands `FetchFailed`.
- [ ] `borg/src/pipeline.rs:193-195, 248` — `terminal_failure_stage` defaults everything to `FetchFailed`; Gate-1/Gate-2 `bail!`s surface as `FetchFailed` instead of `QualityBlocked`, and non-URL handlers never set a stage. Fix: typed `Failed` results from the gates; per-handler error classification.
- [ ] `borg/src/routes.rs:163-169` + `replay.rs:236-239` + `lib.rs:789-794` — the daemon always answers `Queued`, which replay maps to `(0,0)`: `ReplayReport` success/failure counts are structurally dead, and the documented sequential pacing silently became enqueue-everything (unbounded queue depth, adjacent to the 2026-05-12 fanout incident). Fix: add the auth-gated `GET /trace/{trace_id}` endpoint (see API Design — direct DB reads don't work from client hosts); replay/reingest poll it for the terminal state before advancing to the next entry, restoring both accounting and pacing (with a per-trace polling ceiling of `hard_timeout + watchdog grace` so a crashed daemon can't hang replay forever).
- [ ] Receipts `degraded` column + `sb borg log --degraded` (see Data Model); update `stages/distill.rs` fallback paths to set it; retire the halt-on-hard-distill policy in CLAUDE.md.
- [ ] `borg/src/intake.rs:145` / `receipts.rs:254-256` — the normal door-failure upsert WARN-spams "already present, no-op". Fix: demote to DEBUG via an `expected_existing` flag.
- [ ] `borg/src/telegram.rs:208-237` — disallowed-chat messages still write sidecars into the Syncthing'd vault forever. With Phase 1's deny-all default this shrinks, but: refuse disallowed chats pre-intake the way the HTTP 401 path does (refused ≠ dropped; `routes.rs:33-35` is the precedent).
- [ ] `borg/src/receipts.rs:340` — `promote_stale_to_crashed` is production-dead with a doc warning telling people not to use it. Delete it.

#### Phase 4: Atomic vault writes everywhere
**Model:** sonnet

- [ ] Promote `cortex::summarize::rewrite_note_file`'s tmp+fsync+rename into `vault` as the shared note-write helper (borg's `pipeline/atomic.rs::write_atomic` is the other existing copy — converge on one). The temp file MUST be created in the target's own directory (rename across filesystems fails; `/tmp` is a different mount) with a unique name (rayon-parallel callers in cortex write concurrently).
- [ ] Route every cortex mutation site through it: `frontmatter.rs:281,311`, `tags.rs:120`, `sweep.rs:253`, `migrate.rs:350,495`, `scope.rs:65`, `classify.rs:394,424,457,822,917`, `autotag.rs:129,174`, `quality.rs:176,202`, `duplicates.rs:202,226`, `linking.rs:277`, `naming.rs:211`, `intel.rs:494`.
- [ ] Route the five borg non-URL publish sites through it: `handlers.rs:620, 859, 1068`, `text.rs:161, 334, 731`.
- [ ] Non-URL publishes also silently clobber same-title notes and ignore `_force`: uniquify colliding filenames and wire `force` (classify's `resolve_collision` is the in-repo pattern).

#### Phase 5: Fail-closed defaults & transport security
**Model:** opus

- [ ] `borg/src/config.rs:786-788` — `is_local_host` returns `true` on hostname error, defeating the Signal single-machine pin whose entire purpose is preventing double-ingest. Fix: fail closed (false + ERROR); test.
- [ ] `oracle/src/config.rs:427-429` — CWD `./oracle.yml` fallback lets any project directory silently reconfigure the MCP server. Fix: drop the fallback (matching the vault-root "no silent CWD" rule); log which file loaded at INFO.
- [ ] First-party clients don't send the auth token: `lib.rs:827-848` (ingest), `lib.rs:763-775` (reingest), `replay.rs:246-255`. Enabling `server.auth-token` 401s the CLI hot path. Fix: resolve the configured token and set `bearer_auth` when present.
- [ ] No `DefaultBodyLimit` anywhere: multipart uploads >2 MB get an undocumented 413. Fix: explicit limit sized to supported attachment types.
- [ ] `borg/src/routes.rs:50` — token comparison is not constant-time. Fix: length-check + byte-fold (or `subtle`).
- [ ] `borg/src/extension/sign.rs:60-66` — AMO credentials passed as argv (world-readable in /proc while signing). Fix: `WEB_EXT_API_KEY`/`WEB_EXT_API_SECRET` env vars.
- [ ] `borg/src/lib.rs:1114-1119` — the systemd hardening block omits `~/.local/share/sb` from `ReadWritePaths`; it only works because the user manager isn't enforcing it (verified live). The moment enforcement lands, receipts/signal-state/stages writes all fail. Fix: declare every real write path; and `lib.rs:1087-1088` hardcodes the vault and secrets paths — derive from config.
- [ ] `borg/src/ntfy.rs:43-70` — topic-name-as-only-secret accepts `force: true` from anyone who guesses the topic. Fix: ignore `force` from the ntfy channel; document the reserved-topic+token requirement in the template.
- [ ] `borg/src/routes.rs:269-273` — multipart `tags` comma-split → repeated fields. `routes.rs:301,319,372` — full-upload clone discarded via `let _ =` (only `bytes.len()` was needed) — delete.

#### Phase 6: Schema is law
**Model:** sonnet

- [ ] Add `Digest` and `Review` to `vault::schema::NoteType`; `cortex/src/intel.rs:297,394` writes them via the enum.
- [ ] Delete dead `intel::process_new_notes` (`intel.rs:131-193`, zero callers, sole writer of invalid `status: processed`); keep legacy `"processed"` read arms.
- [ ] `cortex/src/config.rs:250-258` — `SchemaConfig::default()` is empty (validates nothing) and the hand-typed vocabulary has already drifted (missing reddit/image/pdf/audio/document/code/entity). Fix: build defaults from `Domain::all()/NoteType::all()/Origin::all()/Status::all()/Method::all()`; config remains an override.
- [ ] `cortex/src/quality.rs:37` — `EXCLUDED_TYPES: &["digest", "review", "daily", "system"]` → enum-derived.
- [ ] `borg/src/markdown.rs:103-115` — `ContentType` → raw type strings → map through `NoteType::as_str()`.
- [ ] `cortex/src/summarize.rs:340-349` — `kind_from_type` string matching → `NoteType::from_str` + enum match.
- [ ] `borg/src/types.rs:209-233` — parallel `IngestMethod` enum shadows `vault::schema::Method` (exists only because borg doesn't enable vault's `schemars` feature). Fix: enable the feature, use the vault enum, delete the shadow.
- [ ] Replace scattered string literals with enum `as_str()`: `"assisted"` (`summarize.rs:397`, `classify.rs:811`, `autotag.rs:47`, `entities.rs:90`), `"unread"` (`classify.rs:795`, `autotag.rs:46,147`, `intel.rs:151`).
- [ ] `vault/src/hygiene.rs:31-32` — `DOMAIN_ALIASES` maps Inbox → `"inbox"`, which `Domain::from_str` rejects; drop the alias rows (inbox is a location, not a domain).
- [ ] `vault/src/search/stats.rs` + `cold.rs` — `'daily'`/`'system'`/`'unread'`/`'starred'` SQL literals → enum-driven (the `stale_embedding_targets` IN-clause generation is the in-repo pattern).

#### Phase 7: Error-handling integrity
**Model:** sonnet

- [ ] Add a vault SQLite helper mapping `Err(QueryReturnedNoRows) → Ok(None)` and propagating everything else; replace the `.ok()` conflations at `search/query.rs:125-303`, `search/index.rs:23-30`, `search/graph.rs:171-206`, `vector.rs:267-276`, `stats.rs` — under writer contention, `SQLITE_BUSY` currently reads as "row doesn't exist" (INSERT-on-existing-PK, false missing notes, dropped edges). Same family: `filter_map(|r| r.ok())` row drops get a WARN.
- [ ] `oracle/src/eval/cache.rs:88-113` — `JudgmentCache::get` swallows real DB errors as cache misses (re-buying every LLM judgment). Same fix shape.
- [ ] Silent-swallow sites get WARNs: malformed frontmatter → empty metadata (`vault/src/frontmatter.rs:254-255`), `CliConfig::load` parse failures (`paths.rs:202`), unparseable tags JSON (`graph.rs:341`), `generate_tags` errors discarded at five borg call sites (`pipeline.rs:541`, `handlers.rs:591,827,1041`, `text.rs:129,699`).
- [ ] Timezone parse fallback is silent and septuplicated (`pipeline.rs:281`, `handlers.rs` ×3, `text.rs` ×2, `backfill.rs:363`): validate once at config load, share one helper.
- [ ] `borg/src/fabric.rs:112-137` — `fetch_article` logs nothing when fabric/markitdown fail. Fix: log each tool's exit status + stderr preview.
- [ ] `vault/src/search/stats.rs:472` — `classify_stats` interpolates caller-supplied `domain` raw into SQL (the only unparameterized value in the crate). Parameterize.
- [ ] Typed `FabricError::Timeout` in vault replaces `msg.contains("timed out")` string-matching at six distiller sites (`article.rs:77`, `repo.rs:97`, `thread.rs:82`, `image.rs:85`, `video.rs:313`, `voicenote.rs:291`).
- [ ] `cortex/src/embed.rs:295` — daemon lock detection by error-message substring → typed error.

#### Phase 8: Bloat decomposition
**Model:** sonnet

Do this BEFORE the dedup/refactor phases touch these files. The gate is 1500; never raise it.

- [ ] `borg/src/config.rs` (1489) — extract `config/tests.rs` (the inline test mod is most of the headroom) and split the per-transport config structs into `config/` submodules.
- [ ] `oracle/src/server.rs` (1485) — split the pipeline machinery into `server/pipeline.rs`.
- [ ] `sb/src/cli/checks.rs` (1178 incl. inline tests) — extract `checks/tests.rs`.
- [ ] Advisory (>1200): `borg/src/audit.rs` (1397), `borg/src/lib.rs` (1362), `cortex/src/classify.rs` (1257) — decompose along existing seams (audit kinds, lib daemon-vs-cli, classify tiers).

#### Phase 9: Cross-crate dedup & dependency hygiene
**Model:** opus

- [ ] `distillers::parse` module: one `strip_fences` (FIXING the bug — only search for a closing fence when an opening fence was actually stripped; today unfenced output is truncated at any embedded ```), one `approx_tokens`, one set of `PatternYaml`/`PatternClaim`/`PatternLink`, the chunker + `find_boundary` + `call_fabric` + `build_distilled` + `ReduceYaml` shared video↔voicenote. Port the fence tests so all consumers are covered (today only 3 of 6 copies have tests).
- [ ] `vault::frontmatter::split_raw` replaces the five ad-hoc splitters: `borg/src/replay.rs:86`, `borg/src/migrate.rs:242`, `borg/src/audit.rs:767`, `borg/src/backfill.rs:96`, `cortex/src/migrate.rs:362`.
- [ ] One `truncate_input` (`cortex/src/llm.rs:77` vs `cortex/src/fabric.rs:47` are byte-identical incl. tests).
- [ ] One `update_wikilinks_for_moves` (`naming.rs:167`, `migrate.rs:180` — dead in practice, `classify.rs:878`).
- [ ] `vault/src/ledger.rs:118-143` — positional column parsing in direct violation of the named-columns rule, with `table.rs` (whose doc header cites the ledger as the cautionary tale) unused in the same crate. Migrate to `table::parse_table`; use `table::escape_cell` in `append_entry` (a `|` in a URL currently shatters the row); take the exclusive lock before `ensure_header_matches` (TOCTOU); delete the duplicated `ensure_header_matches`.
- [ ] Export `wait_with_timeout` from `vault::fabric`; fix the stale "mirrors the cortex pattern" comment in `borg/src/fabric.rs:12`.
- [ ] `borg/src/stages/distill.rs` — collapse the seven ~35-line `distill_for_publish_*` clones into one generic `run_distiller(kind, inputs, fallback_id)`; extract a `publish_note()` helper for the 6× copy-pasted handler epilogue (tz, ledger entry, obsidian URL, IngestResult).
- [ ] Extract the ~8× copy-pasted processing→pipeline→result dispatch block in telegram/ntfy/routes into a helper (sinks stay trait-free per policy; the dispatch boilerplate is what drifted).
- [ ] `borg/src/lib.rs:864-875` — fourth ad-hoc desktop sink: dedupe appname/timeout constants with `notify::Desktop`.
- [ ] Expose `borg::probe_telegram()`/`probe_signal()` typed probes; sb's doctor uses them; drop `teloxide`/`signal-rs`/`hostname` from `sb/Cargo.toml`.
- [ ] Hoist all shared deps to `[workspace.dependencies]` (rusqlite ×3, url ×3, thiserror skew 2.0.18/2.0, schemars skew, tempfile ×5, teloxide+features ×2, rmcp ×2, tracing-subscriber ×2, serial_test ×2).
- [ ] `cargo remove` unused: borg `colored` + `env_logger`, cortex `env_logger` + `which`, sb `tracing`.
- [ ] Add `[workspace.lints]` (+ `lints.workspace = true` per crate); add deliberate `[profile.release]` and `[profile.dev.package."*"]` opt-levels for the candle/libsignal weight (or record that defaults are intentional).
- [ ] Delete the test-only `borg/src/markdown.rs:296 sanitize_filename` wrapper; single `mode_label` fn in oracle; `queries.rs:54` uses `judge::MAX_SCORE`.
- [ ] `borg/src/pipeline/handlers.rs:196-201` — `let _ = slide_summary;` (computed via a clone, never used) and `let _ = use_fabric;` (pointless suppression): delete the dead bindings and the unused clone.

#### Phase 10: Oracle correctness & MCP surface
**Model:** opus

- [ ] Fix the four sites telling MCP clients the default mode is hybrid (`server.rs:767`, `server.rs:1469-1472`, `tools.rs:67-69`, `tools.rs:18-19,37-38`) — the no-mode default is the operator-configured pipeline (vector-first, eval-best at 0.876 vs 0.799 nDCG); an agent "asking for the default" explicitly currently gets the worse path. Also the stale "Phase A6" comment.
- [ ] `ingest_history` limit (see API Design).
- [ ] `server.rs:596` — `pipeline_graph_paths` bypasses the documented `MAX_EXPAND_HOPS` clamp for configured hops. Clamp.
- [ ] `server.rs:509-510` — single-enabled-method passthrough ignores the method's fusion weight (weight 0.0 means "out" with two methods, "full strength" alone). Route through `reciprocal_rank_fusion_weighted` for consistency.
- [ ] `invalid_params` (not `internal_error`) for empty query; `direction`/`quality` become schema enums.
- [ ] `server.rs:1160-1171` — `find_similar` filters domain/self AFTER the limit (can return 0 with matches present). Over-fetch then filter.
- [ ] `lib.rs:60` — watcher reindex silently stops forever on a poisoned mutex (no log). Log it; and index only `change.changed_paths` via `index_one` instead of a full vault walk under the lock.
- [ ] `eval/cache.rs:19-23` — `DefaultHasher` is not stable across Rust releases; a toolchain bump silently invalidates the whole judgment cache. Use a pinned-constant hasher (FNV-1a).
- [ ] `server.rs:60-145` — the 19-arm `dispatch()` mirror of the tool router has no parity guard. Add a test asserting every router tool dispatches without "unknown tool".
- [ ] `server.rs:652` — `maybe_rerank` hardwires the candle loader, making the head/tail split, probe latch, and budget branch untestable despite `MockReranker` existing for exactly this. Inject the reranker; unit-test the stage.
- [ ] `vault/src/search/rerank.rs:69-77` — the latency projection divides by threads, but candle runs ONE batched forward already using all cores: the probe can pass and the real batch blow the budget by up to threads×. Project `per_pair_ms * n` for the candle backend (or probe a small real batch).
- [ ] `sb/src/logger.rs:184-204` — no `tracing_log::LogTracer` bridge: under `sb oracle serve`, every `log::*` record from vault (watcher, index warnings) is silently dropped. One line + dep.
- [ ] `oracle/src/eval.rs:126` — relative `eval-cache.db` fallback (same banned class as the logger fallback). Resolve via data dir.
- [ ] Drop the two `#[allow(dead_code)]`s (`server.rs:33-34` — verify the macro consumes it; `vault/src/watcher.rs:54-55` — rename to `_watcher` per the drop-guard carve-out).

#### Phase 11: Cortex governance correctness
**Model:** opus

- [ ] `classify.rs:31-34` — classify runs a full `index_vault` (writing oracle's `notes` table) on every invocation, violating the documented one-way data flow and making cortex+oracle concurrent cross-process writers of the same tables. Drop the call; oracle's watcher refreshes the index.
- [ ] `classify.rs:77` — default Tier-2 pattern `cortex_classify` does not exist (the file is `obsidian-classify.md`); Tier-2 LLM classification has been silently dead on the live system. Fix the default; add a doctor/bootstrap check that configured patterns resolve to files.
- [ ] `migrate.rs:108` and `naming.rs:155` — `fs::rename` clobbers existing destinations (data loss). Bail or suffix when the destination exists.
- [ ] `summarize.rs:160-163, 377-390` — `--resume` checkpoints "last completed" from concurrent completion order then skips everything up to it in scan order: failed/in-flight notes before the checkpoint are never retried; a moved checkpoint note makes resume a silent no-op. Checkpoint a set of completed paths; missing checkpoint path = start fresh with a warning.
- [ ] Per-note `?` aborts whole runs: `classify::apply_classify` (`classify.rs:391-477`), `tags::apply_tags` (117), `scope::apply_scope` (62), `frontmatter::apply_frontmatter` (281,311), `duplicates::apply_duplicates` (186,224). On a Syncthing'd vault, a note deleted between scan and apply is routine. Per-note match + WARN with the path; continue.
- [ ] `linking.rs:116-146, 298` — `lint_linking` is O(notes × titles) with the full body-lowercase + offset map rebuilt per pair; the daemon runs it (lint then apply) every sweep. Hoist the per-note build out of the term loop (or Aho-Corasick over all terms).
- [ ] `daemon.rs:381-635` — cycle detection records placeholder `["__applied__"]` so the `SweepFingerprint` oscillation design degenerates to "one applied fix disables periodic sweeps until a watcher event". Record real file lists and compare consecutive fingerprints (the implemented-but-unreached design).
- [ ] `config.rs:661-664` — `enabled_actions()` returns all configured actions regardless of `enable`; rename to `configured_actions()` and fix the doc.
- [ ] `tags.rs:109-110` — unconditional sort+dedup reorders the user's tag list on any fix; preserve first-seen order.
- [ ] `scope.rs:164` — non-scalar set values write Rust `Debug` repr into frontmatter; serialize via `serde_yaml` or reject at config load.
- [ ] `sweep.rs:137,139,226` — sweep config paths bypass tilde expansion (and use `shellexpand` where used at all): make them `PathBuf` with `deserialize_tilde_pathbuf`.
- [ ] `embed.rs:697-702`, `daemon.rs:641,763,799` — raw `dirs::*` calls and the embed lock at `~/.local/share/cortex/` (outside the `sb/` namespace). Route through `vault::paths` (new `cortex_lock_path()` under `sb/cortex/`).
- [ ] `daemon.rs:690-748` — intel is scheduled by BOTH in-daemon timers and installed systemd timers. Keep the in-daemon scheduler; stop installing the timers; document.
- [ ] `classify.rs:832-878` — suffix-collision moves trigger vault-wide wikilink rewrites pointing `[[foo]]` at `[[foo-2]]` (almost certainly wrong); skip wikilink rewriting for suffix-collision moves. `existing_note_has_source` matches only the quoted form within 2048 bytes — match unquoted too.
- [ ] `config.rs:530` — `IntelConfig` pins `claude-opus-4-20250514`, silently overriding `llm.model`. Default to the shared `llm.model`.
- [ ] `daemon.rs:55-57` — `daemon --stop` is a log-only no-op the user never sees; have sb print the instruction.

#### Phase 12: Vault polish
**Model:** sonnet

- [ ] `canonical.rs:161-169` — `filter_and_cap` globally sorts before capping, destroying the documented mapping > exact > segment priority. Sort within tiers (`(tier, tag)` key).
- [ ] `detail.rs:106-112` — "first H2 section" fallback reads from a HashMap: nondeterministic summary → embedding churn across reindexes. Order-preserving section tracking; also handle duplicate H2 names and skip `## ` inside fenced code blocks (reuse the fence-skipping `extract_wikilinks` already does).
- [ ] Per-call regex/dictionary rebuilds → `LazyLock`: `search.rs:296` (wikilink regex, ~2.3k compiles per pass), `search.rs:261` (stop-word set), `canonical.rs:54-66` (substring dictionary), `distillers/src/idea.rs:71`.
- [ ] O(n·m) scans → HashSet: `search/index.rs:241-253` (`remove_stale_notes`, ≈5M string compares per reindex), `watcher.rs:139-144` (`pending.contains`, quadratic during Syncthing bulk sync).
- [ ] `search/index.rs:6-53` — wrap `index_vault` (and stale-note removal) in one transaction (2.3k autocommits today; readers see a partially-built index).
- [ ] `frontmatter.rs:62-68` — non-string YAML keys roundtrip as `Number(2023)` debug renderings; route keys through `scalar_to_string`.
- [ ] `frontmatter.rs:249` — `find("\n---")` matches inside multi-line YAML values; require a full delimiter line.
- [ ] `search/graph.rs:521-550` — `expand_graph` lets the same node enter the next frontier multiple times per hop (multiplicative waste in dense regions); dedup at push time.
- [ ] `search/schema.rs:8-42` — FTS5 `content_rowid` rides an implicit rowid that VACUUM may renumber. Document the VACUUM prohibition next to the DDL (schema change not warranted yet).
- [ ] `fabric.rs:188-212` — env-mutating test without the static-Mutex `ENV_LOCK` pattern; add it.
- [ ] `trace.rs` — doc claims "guarantee uniqueness" on a 24-bit truncation; fix the doc (collisions ~4k IDs).
- [ ] `note.rs:11-20` — `Note` carries both `raw` and `body` (near-double memory on full-vault scans); audit consumers and drop `raw` if unused.

#### Phase 13: sb CLI compliance & scripts
**Model:** sonnet

- [ ] `cli/borg.rs:40,50` — `--tags` `value_delimiter = ','` → `num_args = 0..` (hard CLI rule; `audit --fix` in the same crate is the model).
- [ ] `cli/cortex.rs:115,137` — `ignore_case = true` on `--format`/`--scan` enum flags (verified failing today on `--format JSON`).
- [ ] `cli/cortex.rs:367-371` — the `Daemon(_)` arm's silent CWD fallback covers `--start`/`--install`, which DO touch the vault (bakes `.` into the unit / watches `.`). Restrict the fallback to status/stop/uninstall; propagate the error otherwise.
- [ ] `logger.rs:166-171` — `dirs::data_local_dir().unwrap_or(PathBuf::from("."))` is the banned fabricated-fallback; `.expect(...)` per the `vault::paths::config_root` pattern.
- [ ] `cli/borg.rs:58-63,321` — dead `--dry-run` on `borg migrate` (parsed, discarded; `--dry-run --apply` applies). Delete the flag (`--apply` is the gate, per the CLI rule).
- [ ] `cli.rs:34` — help still advertises excised `dlq`/`intake` verbs; update.
- [ ] `cli.rs:19-20` vs `logger.rs:174-177` — documented log-level precedence inverted in code (`root.or(sub)` → `sub.or(root)`).
- [ ] `cli/checks.rs:846` — suggests nonexistent `sb doctor signal`; say `sb doctor`.
- [ ] `cli/oracle.rs:46-47` — `--queries` default is repo-relative; note it in help or resolve via repo root.
- [ ] `cli/borg.rs:885`, `cli/oracle.rs:259` — `std::process::exit(1)` inside print helpers; return a typed failure and map in `main`.
- [ ] `cli/borg.rs:248,265` + `borg/src/lib.rs:431,943` — dead `verbose` parameter hardcoded `false`, underscore-suppressed on the lib side; remove end-to-end.
- [ ] Add template-parse tests: `serde_yaml::from_str::<Config>(TEMPLATE)` ×3 (byte-identity checks alone won't catch struct drift).
- [ ] Stale "14 fabric patterns" (`checks.rs:335`, `.otto.yml:233` — actual: 17); `migrate-receipts` `usage()` greps `^# ` and leaks implementation comments into `--help`.
- [ ] `oracle.yml` `watcher.enable` → `enabled` with a serde alias (vocabulary consistency).
- [ ] `sb/build.rs:17-18` — also watch `.git/packed-refs` (stale `GIT_DESCRIBE` after `git pack-refs`).
- [ ] Help-text gaps (`DaemonArgs`/`HotkeyArgs`/boolean flags render blank; `--status` help omits `crashed`); audit summary omits the sixth kind (`github-creator-missing`).
- [ ] `bin/migrate-receipts`: Steps 3+4 write to the DB under `--dry-run` (wrap in BEGIN/ROLLBACK); line 178's mojibake glob (`*ǵ04*` for 🔄) silently skips rows; `--exclude-dir` with an absolute path never excludes (use the basename); use `/usr/bin/tail` (Rust `tail` shadows it).
- [ ] `borg/clients/hotkey/obsidian-borg-capture.sh:43` — invokes `obsidian-borg`, retired by the 2026-05-19 unified-sb refactor; and under `set -euo pipefail` the command-substitution failure aborts before `EXIT_CODE=$?`, so the `Failed:` notification branch is dead. Fix: `sb borg ingest "$URL"` inside `if RESULT=$(...); then`.

#### Phase 14: Docs/contract truth & rule sweeps
**Model:** sonnet

- [ ] `borg/clients/AGENTS.md:11` — instructs `keepalive: true`, the exact known silent-capture-loss bug `popup.js` forbids. Correct it with the why.
- [ ] `distillers/AGENTS.md` + `vault/src/distilled.rs:44-50` — both still say URL kinds leave `transcript: None`; Phase B2 reversed that (a literal reader would reintroduce the regression the tests guard against). Update both; document the top-level `github:` key as the deliberate exception to the `cortex-*` naming contract in `RenderedDistilled`'s docs and AGENTS.md.
- [ ] CLAUDE.md: retire the halt-on-hard-distill/DLQ language (replaced by the receipts `degraded` flag from Phase 3); update the `project-halt-on-hard-distill` memory.
- [ ] Stale comments: `notify.rs:107,136` "500ms" (constant is 3000); the dead `AssertUnwindSafe` wrapper in `telegram.rs:584-592` (delete it).
- [ ] Inline `#[cfg(test)] mod tests` extraction sweep (rule: extract on sight) — vault: schema, frontmatter, note, config, canonical, hygiene, detail, fabric, ledger, trace, watcher; borg: pipeline/inflight, pipeline/atomic, youtube, ocr, quality, extraction, backoff, ntfy, discord, health, extension/schema (config.rs done in Phase 8); cortex: the 18 older modules (frontmatter, sweep, tags, migrate, classify, daemon, ...); sb: checks (Phase 8).
- [ ] Record the deliberate HTTP-stack split in `cortex/AGENTS.md`: borg uses async `reqwest`, cortex uses blocking `ureq` for its one sync LLM POST — intentional (sync loop, lighter dep), now written down instead of looking like drift.
- [ ] Reconcile the rules conflict the review surfaced: rust.md bans `dirs::config_dir()`/`data_local_dir()` in favor of XDG helpers, while CLAUDE.md explicitly sanctions the `dirs::* + .expect(...)` pattern (`vault::paths::config_root`). Amend rust.md (in `~/repos/scottidler/claude`) with the second-brain carve-out so future sessions stop flagging it.
- [ ] Entry-DEBUG logging sweep per logging.md (params at entry, outcome at exit; previews for large payloads): vault `fabric::run_pattern`, `SearchIndex::{search,list_notes,tag_search,index_vault}`, `parse_frontmatter`, `ledger::{append_entry,check_duplicate}`; cortex lint/apply entry points, `sweep::*`, `naming::*`, `duplicates::*`, `llm::complete`; borg `telegram::run`/`discord::run`/`claim_polling_session` (signal::run is the model).

#### Phase 15: Distillers remainder & independent test gaps
**Model:** sonnet

(Fixes in earlier phases land with their own regression tests; this phase holds the distiller fixes not covered by the Phase 9 consolidation, plus the test gaps not tied to any fix.)

- [ ] `borg/src/fabric.rs` has zero tests despite `split_with_overlap`/`find_break_point` carrying fixed-panic comments — add the regression tests.
- [ ] Distillers: partial-chunk-failure path (surviving claims + `partial-chunk-failure` reason), reduce-step failure (concatenated-summaries fallback), multibyte/UTF-8 chunker regression (the comments cite the exact panic; nothing pins it), fence-strip tests for all consumers post-consolidation, frontmatter-additions escaping tests (backslash/newline/colon).
- [ ] `distillers/src/video.rs:275-296`, `voicenote.rs:254-276` — the map-reduce path silently drops chunk tags (`parsed.tags` never read; long videos lose all distiller tags). Union chunk tags, dedup, let `enforce_bounds` cap at 7.
- [ ] `distillers/src/video.rs:167-241` — `chunk` cloned through the result tuple solely to be `let _`-discarded (~32 KB per chunk); carry `(idx, result)` like the voicenote twin.
- [ ] `distillers/src/video.rs:110-114` (+ voicenote) — no empty-transcript guard on the short path; short-circuit `transcript.trim().is_empty()` before burning a Fabric call (the long path already does).
- [ ] `distillers/src/validate.rs:104` — `fallback_distilled` writes the failure reason into `meta.model`; pass the real model in (the reason already lives in `validation.fallback_reason`).
- [ ] `distillers/src/image.rs:88,99,108` + `borg/src/stages/distill.rs` voicenote arm — redundant post-fallback `transcript` re-sets (the fallback already preserves it); delete.
- [ ] `distillers/src/render.rs:124-141` — `push_summary` doesn't demote embedded headings (fallback summaries embed raw transcript heads that can open with `#`) and `push_claims` doesn't sanitize newlines in claim text; run summaries through `demote_headings`, flatten claim newlines.
- [ ] `distillers/src/dispatcher.rs:63,107` — `PassthroughDistiller` constructed in every `Dispatcher`, routed to by nothing since Phase 9c; route a kind to it or drop the field (keep the type).
- [ ] `borg/src/github.rs` — `TRAILING_NOISE` misses `: ; ! ?` so prose punctuation leaks into published `github:` slugs (empirically: `"see github.com/foo/bar!"` → `foo/bar!`); validate each segment against GitHub's name charset (`[A-Za-z0-9._-]`) instead of enumerating noise characters.
- [ ] Oracle: weighted multi-method fusion asserted end-to-end through `run_pipeline` (the "bm25 demoted at 0.3" behavior is never tested), `eval::retrieve` with a real configured-pipeline run, graph seed-weighting/hop-decay at the oracle layer.
- [ ] Borg: `is_local_host` fail-closed test; non-URL handler timeout-bounding test (the existing `pipeline/timeouts.rs` only proves tokio's timeout works on a sleeping future).

## Alternatives Considered

### Alternative 1: Fix only criticals + majors, batch the long tail into "as touched"
- **Description:** Ship Phases 1-5 now; address minors/nits opportunistically.
- **Pros:** Faster to the high-value fixes.
- **Cons:** The long tail is exactly the rule-drift (inline tests, logging gaps, CLI violations) that made half the codebase non-compliant; "as touched" is how it drifted in the first place.
- **Why not chosen:** No-deferments is the operating rule here; every finding gets a phase.

### Alternative 2: Big-bang refactor (consolidate first, fix second)
- **Description:** Do Phase 9's consolidation first so fixes land once in shared code.
- **Pros:** Some fixes (fence-strip, frontmatter splitting) become single-point.
- **Cons:** Delays the 7 criticals — several actively degrade the live system today (watcher silence, dead Tier-2 classify, daemon-applied tag corruption) — behind the riskiest refactoring.
- **Why not chosen:** Criticals are cheap and independent; they go first. Consolidation follows with the criticals already pinned by tests.

### Alternative 3: Rewrite the receipts/replay path around a synchronous ingest mode
- **Description:** Add a `?sync=true` ingest variant so replay gets terminal results in the response.
- **Pros:** Simpler replay accounting.
- **Cons:** Reintroduces long-held HTTP connections the detached design deliberately removed; the receipts DB is already the authoritative store.
- **Why not chosen:** Polling receipts for the terminal state restores both accounting and pacing without changing the wire contract.

## Technical Considerations

### Dependencies

No new runtime dependencies except: `tracing-log` (oracle log-bridge, one line), possibly `aho-corasick` (cortex linking perf; already in the dependency tree transitively via regex). Several dependencies are removed (borg `colored`/`env_logger`, cortex `env_logger`/`which`, sb `tracing`, sb's transport probes' `teloxide`/`signal-rs`/`hostname`). All shared deps hoist to `[workspace.dependencies]`.

### Performance

Strictly improvements: LazyLock regexes (~2.3k compiles/pass removed), HashSet scans (≈5M string compares/reindex removed), one transaction around `index_vault`, linking lint dropping from O(notes×titles) full-body rebuilds, oracle indexing only changed paths. The atomic-write helper adds fsync per note write; backfill.rs already documents sequential writes as the mitigation for fsync contention — sweeps follow the same pattern.

### Security

Phase 5 is the security phase: Telegram deny-all default, `is_local_host` fail-closed, constant-time token compare, AMO creds via env, body limits, ntfy `force` ignored, systemd `ReadWritePaths` matching reality. None of these change the threat model; they make the implemented model hold.

### Testing Strategy

Every fix in Phases 1-13 lands with a regression test in the same commit (TDD where the bug is reproducible: write the failing test first — the watcher two-batch test, the column-0 tags test, the stage-0 receipts test, the YAML-escaping tests are all of this shape). Phase 15 closes the remaining identified gaps. `otto ci` (lint + bloat + check/clippy/fmt + test) gates every phase; verify by exit code.

### Rollout Plan

Per-phase commits, shipped back-to-back via the standard flow (`otto ci` → commit → `bump` → `otto install` + `systemctl restart borg cortex`; full `otto deploy` only if the extension is touched — it isn't). Two coordination notes:

1. **Telegram deny-all (Phase 1)** is behavior-changing: before deploying, verify `allowed-chat-ids` is populated in the live `borg.yml` on desk — if it's empty, Telegram ingest stops at deploy. `sb doctor` gains the empty-allowlist warning in the same change.
2. **Receipts `degraded` column (Phase 3)** rides the existing on-open migration mechanism; old rows default to 0. No backfill needed. Snapshot `receipts.db` before the first post-change daemon start.
3. **No extension re-sign needed:** nothing changes `IngestRequest` (the multipart `tags` change is additive at the wire and the JSON routes are untouched), so the standard `otto install` + restart flow suffices; `otto deploy`'s AMO round is not required.
4. **Reindex after Phase 6:** the 76 notes currently indexed with empty `note_type` only refresh on mtime change; run the oracle `reindex` tool (or touch-free full reindex via `sb oracle`) once after the `Digest`/`Review` variants land so they pick up real types.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Telegram deny-all locks out live ingest if config lacks the chat id | Low | Med | Pre-deploy config check on desk; doctor warning ships in the same commit |
| Atomic-write conversion changes mtime/fsync behavior under Syncthing mid-sweep | Low | Med | Same tmp+rename pattern summarize/backfill already use in production; per-note tests |
| Dedup refactors (Phase 9) regress behavior the copies had quietly diverged on | Med | Med | Port ALL existing tests to the shared module before deleting copies; diff the copies first to surface intentional divergence |
| Receipts schema migration fails on an old DB | Low | High | Migration is additive (one defaulted column) via the existing mechanism; snapshot receipts.db before first deploy (per the migration-verification rule) |
| `wait()` fail-fast (Phase 1) turns a previously-tolerated transient task error into a restart loop | Low | Med | Transports already own reconnect/backoff internally; only errors that escape the supervisor loop (by design fatal) abort; Restart=always recovers |
| Tier-2 classify suddenly going live (pattern fix) changes inbox behavior | Med | Low | It restores the designed behavior; ambiguous notes were silently held for review before — watch the first daemon cycle's classify report |

## Open Questions

- [ ] None — all decisions are made above. (Notably: no new `Status` variant, `degraded` as a receipts column rather than a new status, in-daemon intel scheduling over systemd timers, FTS rowid documented rather than re-keyed.)

## References

- Review source: eight-agent parallel review, 2026-06-09 (this session); empirical confirmations: watcher panic, YAML escaping, fence-strip truncation, slug punctuation.
- `docs/design/2026-06-06-configurable-retrieval-pipeline.md` (oracle pipeline contract)
- `docs/design/2026-06-03-receipts-log-legacy-markdown-excision.md` (receipts as sole failure store)
- `docs/design/2026-05-24-signal-as-borg-transport.md` (single-machine pin rationale)
- CLAUDE.md global invariants; `~/repos/.claude/rules/{general,cli,logging}.md`
