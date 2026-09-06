# Implementation Notes: Discovery Remediation

Design doc: `docs/design/2026-09-05-discovery-remediation.md`

## Phase 0: Ship the in-flight --maxTokens work (R7)

### Design decisions
- Committed the design doc first, standalone, before touching any tracked
  file — `docs(design): discovery remediation` (`3c6de89`) — per the doc's
  explicit ordering instruction.
- Folded two clippy fixes into the Phase 0 commit rather than a separate one:
  `chunks_exact` -> `as_chunks::<4>()` in `vault/src/search/vector.rs:162,325,388`,
  and `#[allow(clippy::result_unit_err)]` on `borg::notify::Telegram::processing`
  and `borg::notify::Signal::processing` (`borg/src/notify.rs:114,432`). Both
  lints are newly enforced by the local `rustc`/`clippy` 1.98.0 toolchain
  (`.github/workflows/release.yml` pins CI to 1.96.0) and were failing `otto
  ci` on files untouched by the 9-file `--maxTokens` diff. Fixing them at the
  correct minimal seam (mechanical rewrite; `#[allow]` with a rationale
  comment rather than redesigning the public `Result<(), ()>` API) was
  required to get `otto ci` green at all, so it rode in this commit instead of
  blocking the phase.
- Appended the exact paragraph specified in the doc's Phase 0 bullet to
  `docs/design/2026-08-30-video-distill-token-budget.md` Resolved Decisions,
  verbatim, and touched nothing else in that file.

### Deviations
- **Ordered deferral, not a spec gap:** `bump && otto deploy` was explicitly
  withheld per the team lead's instruction. Those are held for a finalization
  checkpoint after all 16 phases land, not run per-phase. Consequently the
  third success criterion (`sb doctor | grep -c 'maxTokens'` >= 1 on the
  *deployed* binary) is DEFERRED-TO-DEPLOY: it cannot be true until that
  finalization deploy happens.
- Two files outside the doc's named 9 (`borg/src/notify.rs`,
  `vault/src/search/vector.rs`) are in this commit. Both changes are
  toolchain-drift clippy fixes, not behavior changes, and were necessary for
  `otto ci` to pass under the locally installed 1.98.0 toolchain regardless
  of this phase's diff (verified: the two lints fire on baseline `f97718f`
  code untouched by the 9-file diff). No other phase in this doc claims these
  two files.

### Tradeoffs
- `#[allow(clippy::result_unit_err)]` vs. introducing a real error enum for
  `notify::Telegram`/`notify::Signal::processing`: chose the allow. A typed
  error would ripple into every caller of both `processing` fns across
  `borg/src/pipeline*`, which is out of Phase 0's scope and belongs to
  whichever phase (if any) later touches notify's error contract.

### Open questions
- None.

## Phase 1: Delete the passthrough stub (S5)

### Design decisions
- `git rm distillers/src/passthrough.rs distillers/src/passthrough/tests.rs`
  and removed both the `pub mod passthrough;` and
  `pub use passthrough::PassthroughDistiller;` lines from
  `distillers/src/lib.rs:17,37`.
- Rewrote the dead-code comment at `distillers/src/dispatcher.rs:169-173`
  (the `PassthroughDistiller` mention above the `VoiceNote` match arm) to
  drop the now-nonexistent-type reference, keeping only the still-true
  routing note about `VoiceNote`'s own Fabric-backed distiller.
- Reworded the matching comment at
  `borg/src/stages/distill/tests.rs:136-138` (above
  `distill_stage_handles_image_through_image_distiller`) the same way: it
  named `PassthroughDistiller` as what Image *used to* route through; the
  comment now just states the current routing and fallback behavior.
- Retagged
  `config/eval/distill-fixtures/idea/linker-edge-from-capture-note/distilled.yml:12`
  `meta.extractor` from `distill-passthrough-v1` to `distill-idea-v2` since
  the fixture's `IdeaDistiller` is the live extractor for that path.
- Left `borg::stages::extract::PassthroughExtractor`
  (`borg/src/stages/extract.rs:25`) untouched — a distinct, live Stage-1
  extractor, out of this phase's scope per the doc's explicit instruction.

### Deviations
- None.

### Tradeoffs
- None — this phase is a pure deletion with no design choice beyond what
  the doc specified.

### Open questions
- None.

## Phase 2: AGENTS.md module-map lint (R3)

### Design decisions
- `bin/agents-map` (bash, executable, same `FAIL:`/loop/`exit 1` shape as the
  `bloat` task): forward check extracts backticked ``` `token.rs` ``` tokens
  from every `AGENTS.md` (`grep -oE '`[A-Za-z0-9_./-]+\.rs`'`, trailing
  backtick required so refs like `` `borg/src/service.rs:240` `` — which end
  in a line number before the closing backtick — never match); each token
  resolves by basename via `find <crate> -name <basename> -not -path
  '*/target/*'`, where `<crate>` is the AGENTS.md path's first path
  component. Basename (not path-prefix) resolution is load-bearing: nested
  AGENTS.md files name parent-dir modules (`borg/src/pipeline/AGENTS.md`
  names `pipeline.rs`, which actually lives at `borg/src/pipeline.rs`, a
  sibling of the `pipeline/` dir, not inside it) and `sb/AGENTS.md` names
  `cli/*.rs` files by bare name.
- Reverse check runs only when `agents_md` path equals `<crate>/AGENTS.md`
  exactly (crate-root, not nested): for each `<crate>/src/*.rs` at
  `-maxdepth 1`, excluding `lib.rs`/`main.rs`/`tests.rs`/`testutil.rs`, a
  plain `grep -qF <basename>` against the AGENTS.md file must hit; `FAIL:
  <agents.md>: <file> undocumented` otherwise. Plain substring match (not
  backtick-anchored) per the doc's literal wording ("must appear ...
  somewhere in that file"); verified no false-positive substring collisions
  across the current six crate-root files.
- `.otto.yml`: new `agents-map` task (`bin/agents-map`, one line), wired into
  `ci: before` as `[lint, bloat, agents-map, check, test]` (after `bloat`,
  before `check`, per the doc).

### Deviations
- **Expected drift, not a defect, from the doc's `f97718f` measurement.**
  Phase 1 (`4eaf9d5`) deleted `distillers/src/passthrough.rs` after the doc's
  numbers were taken, so the forward check now reports **two** misses instead
  of the doc's one: `cortex/AGENTS.md: hygiene.rs not found` (the doc's
  original catch) and `distillers/AGENTS.md: passthrough.rs not found` (new,
  caused by Phase 1). Phase 3 already plans to remove that row, so no action
  taken here. The reverse check independently found exactly 20 undocumented
  files, matching the doc's count precisely (9 in `borg/AGENTS.md`, 6 in
  `cortex/AGENTS.md`, 2 in `distillers/AGENTS.md`, 1 in `oracle/AGENTS.md`, 2
  in `vault/AGENTS.md`) — see the exact FAIL lines in the phase report.
- Same effect, correct seam: the doc's Phase 2 bullet writes the reverse
  check as "appear by basename somewhere in that file" without specifying a
  match mechanism; implemented as a literal substring `grep -F`, which is the
  simplest faithful reading and matches every basename actually present with
  no observed false hit or false miss against the current tree.

### Tradeoffs
- Substring `grep -F` vs. requiring backtick-wrapped tokens for the reverse
  check: chose substring, per the doc's literal "appear ... somewhere" wording
  (not "appear backticked"). A backtick-anchored variant would be stricter
  but is not what Phase 2 specified; Phase 3 is free to backtick every row it
  adds regardless of which reading is enforced.

### Open questions
- None: the design doc's own success criteria for this phase are "exit 1
  here, exit 0 after Phase 3", which is exactly what was observed.

## Phase 3: Repo doc drift (F10)

### Design decisions
- Root `CLAUDE.md:16` distillers list corrected to `(article, repo, video,
  thread, image, voicenote, session, idea)`, matching the eight files actually
  present in `distillers/src/` post-Phase-1.
- Root `CLAUDE.md:25` corrected to `borg::service::install_systemd`
  (`borg/src/service.rs:240`), plus the pure renderer `render_systemd_unit`
  at `:186` — verified both line numbers against source.
- Root `CLAUDE.md:52` and `vault/AGENTS.md:17,23` Distilled contract corrected
  to `{ summary, tldr, slug, enumeration, key_ideas, claims, tags, links,
  kind_specific, meta, transcript }`, matching `vault::distilled::Distilled`'s
  actual field order in `vault/src/distilled.rs`.
- Root `CLAUDE.md:55` L2 patterns line rewritten to the doc's prescribed
  prose ("chunk/reduce triples for article, video, thread, session,
  voicenote; `distill-repo`, `distill-image`; nine support patterns; 26
  files") — verified by listing `borg/patterns/` (26 files: 5 kinds x 3
  chunk/main/reduce = 15, + `distill-repo` + `distill-image` = 17, + 9
  support patterns = 26).
- `borg/AGENTS.md` Entry Points: added `GET /trace/{trace_id}`
  (`routes.rs:211`, `trace_state`) and named the auth gate
  (`routes::require_auth`, `routes.rs:49`) as a `route_layer` wired in
  `build_router` (`lib.rs:104-127`) over `/ingest`, `/ingest/file`, `/note`,
  and `/trace/{trace_id}`; `/health`/`/health/audit` stay open — verified
  against the actual router-assembly code, not just the doc's claim.
- Module-map rows added/removed across five AGENTS.md files, one line each,
  written from each module's own doc comment (read every file's header
  before writing its purpose, per instruction — none guessed from filename
  alone):
  - `vault/AGENTS.md`: added `text.rs`, `tombstone.rs`.
  - `borg/AGENTS.md`: added `backoff.rs`, `byline.rs`, `dedupe.rs`,
    `dispatch.rs`, `eval.rs` (+`eval/`), `harvest.rs` (+`harvest/`),
    `readability.rs`, `service.rs`, `thread.rs`.
  - `cortex/AGENTS.md`: removed `hygiene.rs` (file does not exist in
    `cortex/src/`; the doc's own S5-adjacent stale-row target); added
    `association.rs`, `bridge.rs`, `entities.rs`, `graph.rs`, `hub.rs`
    (+`hub/render.rs`, `hub/asymmetry.rs`), `memgraph.rs` under a new
    "Knowledge graph (graph-augmented-memory)" grouping line (the six
    modules share one design lineage, so grouped rather than scattered
    across the existing groups).
  - `oracle/AGENTS.md`: added `eval.rs` (+`eval/`).
  - `distillers/AGENTS.md`: removed `passthrough.rs` (deleted by Phase 1);
    added `session.rs`, `parse.rs`.
  - `sb/AGENTS.md`: no change, per the doc ("none missing").

### Deviations
- None from the doc's Phase 3 bullets. The doc's own note that `f97718f`
  line numbers "confirmed" the routes.rs `:211`/`:49` numbers were re-checked
  against current `HEAD` (98e9543) content, not assumed stale.

### Tradeoffs
- Grouped the six new cortex knowledge-graph modules under one new bullet
  rather than folding them individually into the existing
  Classification/Quality/Lifecycle groups: they share a single design
  lineage (`graph-augmented-memory`, `entity-hub-two-vector-synthesis`,
  `MemGraphRAG`) that none of the existing group labels name, and a future
  reader scanning the module map benefits more from that grouping than from
  matching the shortest possible diff.
- `hub/render.rs` and `hub/asymmetry.rs` documented inline within the
  `hub.rs` row (`hub.rs (+hub/render.rs, hub/asymmetry.rs)`) rather than as
  separate top-level rows: the Phase 2 reverse check only requires top-level
  `<crate>/src/*.rs` files to be documented (nested files are out of its
  scope per the doc), so this satisfies the lint while still surfacing both
  files for a human reader, matching the doc's own bracketed instruction.

### Open questions
- None.

## Phase 4: Empty-slug publish fallback (F1)

### Design decisions
- `borg::hygiene::note_filename(title, trace_id) -> String` (`borg/src/hygiene.rs`):
  `sanitize_filename(title)`, or `format!("untitled-{trace_id}")` when that
  sanitizes to empty. Logs `log::debug!` only on the fallback branch (the
  doc's explicit instruction — this is a two-line conditional formatter, not
  the entry/exit-logged case the function-level debug-logging rule targets).
  Placed beside the `pub use vault::hygiene::{...}` re-export it wraps, so
  the one borg-owned seam over the vault primitive lives next to what it
  wraps.
- Replaced all nine note-publish call sites with `hygiene::note_filename(&title, trace_id)`:
  `borg/src/pipeline.rs:896,986` (both use the in-scope `&str` `trace_id`
  parameter of `process_url_inner`); `borg/src/pipeline/text.rs:170,323,696`;
  `borg/src/pipeline/handlers.rs:798,1027,1243` (variable named
  `note_filename` at these three sites — shadows-by-name only the function
  it calls, not a conflict since the call is `hygiene::note_filename(...)`).
- `borg/src/pipeline/session.rs`: `harvest_slug_stem` grew a third parameter,
  `trace_id: &str`, and both its match arms (previously `hygiene::sanitize_filename(slug)`
  / `hygiene::sanitize_filename(title)`) now call `hygiene::note_filename(slug, trace_id)`
  / `hygiene::note_filename(title, trace_id)` — same slug-then-title
  preference order, each branch's result individually wrapped through the
  empty-fallback seam ("wrap the result", per the doc). Call site at
  (then-)line 512 passes the in-scope `trace_id: &str`. Reworded the doc
  comment at (then-)line 28 from "Both branches pass through
  `hygiene::sanitize_filename`..." to name `hygiene::note_filename` and the
  non-empty guarantee, per the doc's explicit instruction that this phase's
  criterion depends on it.
- Left `borg/src/assets.rs:15,26` untouched (asset names, a different
  contract per the doc) and left `vault::hygiene::sanitize_filename` plus its
  test `sanitize_filename_empty_input_stays_empty` unchanged — the primitive
  is correct, the guard belongs one layer up in borg.
- Tests added to `borg/src/hygiene/tests.rs`: empty title -> `untitled-tg-2280a3`;
  a ten-character U+2500 (box-drawing) title -> same; `"Hello World"` ->
  `hello-world` (negative case, confirming the fallback does not fire for a
  normal title).
- Added a fourth case to `borg/src/pipeline/session/tests.rs`
  (`harvest_slug_stem_falls_back_to_trace_id_when_title_sanitizes_to_empty`)
  covering the new empty-fallback branch now reachable through
  `harvest_slug_stem`, alongside updating its three existing tests for the
  new `trace_id` parameter.
- Integration test `borg/tests/empty_slug_publish_falls_back_to_trace_id.rs`:
  drives the real `borg::pipeline::process_content` entry point (not a unit
  stub) with `ContentKind::Text` carrying a ten-U+2500 title, using the same
  `common::XdgSandbox` / `common::test_config` / permit-pool-init harness the
  other `borg/tests/` regression guards use, and asserts the landed note's
  filename stem starts with `untitled-`.

### Deviations
- None from the doc's Phase 4 bullets — implemented at the seam the doc
  names (`borg::hygiene::note_filename`), same nine call sites, same
  `session.rs` "keep slug-then-title order, wrap the result" shape.

### Tradeoffs
- `harvest_slug_stem` took an added `trace_id: &str` parameter rather than
  computing the fallback at its single call site: the function already owns
  the slug-vs-title decision and is the one place that should own turning
  either result into a guaranteed-non-empty stem, matching the doc's
  "wrap the result" phrasing literally (the wrap happens inside the function
  that produces the result, not after it returns).

### Open questions
- None.

## Phase 5: borg unit log level from config (S2a)

### Design decisions
- `borg::service::render_systemd_unit` (`borg/src/service.rs:186`) now
  derives `log_level` from `config.log_level.as_deref().unwrap_or("info")`
  and interpolates it into `ExecStart=... borg --log-level {log_level}
  daemon --start`, replacing the hardcoded `debug` literal. Mirrors
  `cortex::daemon::render_systemd_unit` (`cortex/src/daemon.rs:896,936`),
  modulo the `Option<String>` vs plain `String` field shape (borg's
  `Config::log_level` is `Option<String>`; cortex's is a non-optional
  `String` with its own default elsewhere).
- Left `borg/src/config.rs:224`'s `log_level: Option<String>` field
  definition untouched: the doc's ask was to make the existing field a live
  knob, not to change its type or add a default at the config layer (the
  `unwrap_or("info")` fallback lives at the render seam, same as every other
  optional daemon knob rendered in this file).

### Deviations
- None. Implemented at the exact line the doc names
  (`borg/src/service.rs:217`, content-relocated by prior phases but the same
  `ExecStart=` line).

### Tradeoffs
- None beyond what's already described above.

### Open questions
- None.

## Phase 6: Rotate the `oracle serve` log (S2b)

### Design decisions
- The drop counter reaches the shutdown line through a process-global probe in
  `vault::logging` (`set_dropped_log_lines_probe` / `dropped_log_lines`,
  `vault/src/logging.rs:116-136`) rather than a `serve()` argument. `sb` owns
  the `NonBlocking` writer and `oracle` writes the shutdown line; `oracle` is
  upstream of `sb`, and the guard lives in `main`, not in the frame that calls
  `oracle::serve`. Threading it would mean a new parameter on `serve` plus a
  field carried through `Cli::run` for one integer. The writer stack is already
  process-global (one `tracing` subscriber per process), so the counter that
  belongs to it lives in the crate both sides depend on. Reads 0 when unset,
  which is exactly right for every synchronous logger path.
- `rotating_non_blocking` (`sb/src/logger.rs`) is a separate seam from
  `init_tracing_to_file` so the rotation test can drive the real writer stack
  without `tracing_subscriber::fmt().init()`, which is process-global and would
  poison every other test in the `sb` binary. Same split vault already uses for
  `rotating_log_writer` vs `setup_logging`.
- `vault::logging::rotating_log_writer` and both rotation constants became
  `pub` instead of sb re-declaring 50 MiB x 5. One rotator, one policy.

### Deviations
- **Success criterion 3 as literally written cannot pass, before or after this
  phase.** `sb oracle serve </dev/null` exits **1**, not 0: rmcp fails the MCP
  handshake on an immediate stdin EOF (`Error: connection closed: initialize
  request`) at `server.serve(transport).await?`, which is before
  `service.waiting()` and therefore before the shutdown line. Verified
  pre-existing by running the stale `~/.cargo/bin/sb` (built from Phase 5 code)
  the same way: also exit 1, same error. Smoked instead by feeding a valid
  `initialize` + `notifications/initialized` on stdin and then closing it,
  which is what the criterion was reaching for (transport ends after indexing,
  clean shutdown): exit **0**, and the last line of `~/.local/share/sb/oracle.log`
  is `2026-09-06T06:10:04.768867Z  INFO oracle: MCP server shutting down
  dropped_log_lines=0`, matching the criterion's regex.
- `tracing-appender` resolved to **0.2.5**, not the 0.2.4 the doc cites.
  `Cargo.lock` delta is +29 lines / 3 new packages: `tracing-appender 0.2.5`,
  `crossbeam-channel 0.5.17` (the one the doc predicted), and `symlink 0.1.0`
  (a 0.2.5 dependency of the time-based `rolling` appender we do not use).
- Test lives at `sb/src/logger/tests.rs` (new `#[cfg(test)] mod tests;` beside
  `logger.rs`), matching `vault/src/logging/tests.rs`; the doc did not name a
  location.

### Tradeoffs
- Rotation test writes a real 52 MiB through the real stack into a tempdir
  (0.05 s observed) rather than parameterising the byte cap for the test. A
  test-only cap would prove a different constant than production uses.
- 1 MiB chunks in the test, not log-sized lines: the lossy channel is bounded at
  128,000 entries, so 54 big writes cannot drop, and the test asserts
  `dropped_lines() == 0` so a future channel-bound change fails loudly instead
  of silently testing a truncated file.

### Open questions
- Two `sb oracle serve` processes (one per Claude Code session) write the same
  `~/.local/share/sb/oracle.log` concurrently, and now both do so through
  `FileRotate`. Whichever crosses 50 MiB renames the file under the other, which
  then keeps writing to the renamed inode until it restarts. Pre-existing for
  every sb log (the `env_logger` `DualWriter` path has the same shape) and not
  in this phase's scope, but this phase is the first to put the rotator under a
  multi-process log.

## Phase 7: Doctor reports cortex lint's frontmatter policy + `.claude`/`templates` ignore (F5)

### Design decisions
- `Report::count_by_rule_prefix` — `cortex/src/report.rs` — new method groups
  violations by the rule-string suffix after a caller-supplied prefix
  (`"frontmatter.required."` -> `{domain, origin, tags}`), so doctor reads
  cortex's own rule-naming convention (`format!("frontmatter.required.{field}")`
  / `format!("frontmatter.enum.{field}")` in `cortex::frontmatter`) instead of
  a second copy of the field list.
- `frontmatter_policy_findings` — `sb/src/cli/checks.rs` — new helper, called
  from `vault_findings`. Loads `cortex::config::Config::load(None)`, resolves
  `vault_root` via `Config::vault_root(None)` (same resolver every other `sb
  cortex` subcommand uses), then calls `cortex::lint(&vault_root, &config,
  &LintOpts { rule: vec!["frontmatter".into()], apply: false, format:
  LintFormat::Human, path: None })` — the CLI's own entry point, not
  `scan_vault` + `lint_frontmatter` directly, so the `vault.exclude`/`include`
  filter (`lintable_notes` in `cortex::lib::lint_with_notes`) applies and
  doctor counts the same set `sb cortex lint` prints (verified: both count
  920/931/289 for domain/origin/tags, see Success criteria below).
- Dropped the old `stats.schema_gaps` Info line from `vault_findings`: doctor
  now emits one frontmatter signal (the policy the daemon enforces), not two
  competing ones (raw index emptiness vs. lint policy).
- `vault/src/search/stats.rs::compute_schema_gaps` — dropped `status` from the
  three-field raw-gap scan (`domain`, `note_type`, `origin` remain); `status`
  is optional per `status-values.md`/`frontmatter.md`, so an empty `status` was
  never a real gap. `stats()` (and therefore oracle's `vault_overview`) keeps
  the field name `schema_gaps` and the other three counts unchanged.
- `cortex::config::Config::load_from_file` — `cortex/src/config.rs` —
  `log::warn!("schema: overrides the enum-derived vocabulary; delete it unless
  you mean to")` when the raw YAML parses to a mapping containing a top-level
  `schema` key, checked via a `serde_yaml::Value` parse of the same `content`
  string before the typed `Self` parse (so the warn fires whether or not the
  block itself is well-formed enough to deserialize into `SchemaConfig`).
- `vault::config::ScanConfig::default().ignore` gained `.claude` and
  `templates` (oracle's full-index scan path,
  `vault/src/search/index.rs::ScanConfig::default()`); `oracle::config::
  default_ignore_dirs` (the watcher's list) gained `.claude` (it already had
  `templates`). The two lists now agree on both directories.

### Deviations
- None. Implemented at the exact seam the doc specifies (`cortex::lint`, not
  `scan_vault` + `lint_frontmatter`); no static/enum exempt list was added
  anywhere — the entity exemption stays a cortex.yml line, deferred to
  Phase 14 as directed.

### Tradeoffs
- `count_by_rule_prefix` returns a `BTreeMap<String, u64>` keyed by the bare
  suffix (`"domain"`, `"origin"`) rather than the full rule string, so a
  caller cannot distinguish `"frontmatter.required.domain"` from a
  differently-prefixed rule that happened to end in `.domain` — acceptable
  because the two call sites in this phase pass prefixes wide enough
  (`"frontmatter.required."`, `"frontmatter.enum."`) that no collision is
  possible under the current rule-naming convention, and the unit test pins
  that convention.

### Open questions
- None.

## Phase 8: Doctor stale-inbox Warn and data-dir section (R5, R6)

### Design decisions
- `SearchIndex::inbox_oldest` — `vault/src/search/stats.rs` — the exact query
  the doc specifies (`WHERE path LIKE 'inbox/%' AND path NOT LIKE 'inbox/.%'
  ORDER BY modified_at ASC LIMIT 1`), returning `Result<Option<(String, i64)>>`.
  Placed beside the pre-existing `inbox_notes` (which does NOT exclude
  `inbox/.%` dotfiles — that method answers a different question and was left
  untouched).
- `INBOX_STALE_SECS: u64 = 48 * 3600` — `sb/src/cli/checks.rs` — placed next to
  `FABRIC_PROBE_TIMEOUT_SECS` per the doc, carrying the scar-tissue rationale
  comment (daemon classifies every 300s; `cortex::classify::mark_needs_review`
  at `classify.rs:963` deliberately leaves a no-signal/low-confidence note
  unclassified rather than guessing, so silence past 48h means a human needs
  to look).
- `inbox_stale_finding` — `sb/src/cli/checks.rs` — new helper called from
  `vault_findings` (reuses the already-open `SearchIndex` handle rather than
  opening a second one). Emits `Finding::ok` when the inbox is empty or the
  oldest note is within the window, `Finding::warn` otherwise.
- `vault::paths::dir_size` — walks with `WalkDir::new(root).follow_links(false)`
  (matching the existing `note::collect_md_paths` convention), summing
  `metadata.len()` over regular files only. A missing/unreadable root
  contributes 0, not an error — this is a doctor Info/Warn signal, not a
  build-breaking check, and errors are `filter_map`'d out.
- New `data dir` doctor section (`data_dir_findings`, registered in
  `all_sections()`): four `Finding::info` lines — `stages` (via
  `vault::paths::borg_stages_dir()`), `receipts.db` (via
  `vault::receipts::receipts_db_path()`, sized as a single file — `dir_size`
  works unchanged on a file path since `WalkDir` over a file yields exactly
  that one entry), `logs` (see below), `oracle` (via
  `vault::paths::oracle_db_path().parent()`). `Finding::warn` when logs exceed
  `DATA_DIR_LOGS_WARN_BYTES` (512 MiB) or the summed total exceeds
  `DATA_DIR_TOTAL_WARN_BYTES` (2 GiB); `Finding::warn("stray backup in oracle
  dir: <name>")` for any top-level file in the oracle dir other than
  `oracle.db{,-wal,-shm}`/`eval-cache.db` (`ORACLE_DIR_ALLOWED_FILES`).
- `sum_matching_files` — `sb/src/cli/checks.rs` — new non-recursive helper for
  the `logs` line specifically: logs land flat under
  `xdg_data_dir()/sb/*.log*` (`.log`, `.log.1`, ... from `FileRotate`, per
  `sb/src/logger.rs::log_path`), not nested per subsystem, so this reads only
  the top-level directory entries and filters by `name.contains(".log")`
  rather than recursing with `dir_size`.
- `oracle_db_path().parent()` is used verbatim (not a new `sb_oracle_dir()`
  constant) so the `oracle` Info line and the stray-file scan need no code
  change when Phase 9 repoints `oracle_db_path()` at `sb/oracle/`: today it
  resolves the legacy `~/.local/share/oracle/` dir; after Phase 9 lands it
  resolves the new location automatically. Phase 9's own "legacy oracle dir
  present" `Finding::warn` is a *separate* new push into this same section
  (using its own `legacy_oracle_dir()`), not a change to any Phase 8 code.

### Deviations
- None. Implemented at the exact seam the doc specifies.

### Tradeoffs
- `dir_size` reports the sum of *logical* file sizes (`metadata.len()`), not
  disk-block usage. Measured on `~/.local/share/sb/borg/stages` (23,963 trace
  dirs, 24,973 files): `dir_size` / the doctor `stages:` line reports 46.9 MB,
  while `du -sh` on the same directory reports 234 MB (matching the design
  doc's own recorded observation, presumably also taken with `du`). Confirmed
  by direct measurement (`find -printf '%s\n' | awk '{s+=$1}'` = 49,198,447
  bytes = 46.9 MB, agreeing with `dir_size`): with ~25k mostly-small files,
  filesystem block-rounding inflates `du`'s block-count view roughly 5x over
  the logical byte sum. This is by design, not a bug — the doc's own spec
  says "walkdir sum" (logical size), and a disk-usage-accurate number would
  need `st_blocks`, which isn't portable and isn't what was asked for.
- Wall-clock cost of `dir_size` over that same stages dir (24,973 files, one
  process instance): **333.8 ms**, measured with a temporary `Instant`/
  `eprintln!` wrapped around the call in a live `sb doctor` run, then removed
  before commit. Under the doc's 1-second concern threshold — no depth cap or
  caching implemented (per instructions: measure and record only, don't
  implement a cap in this phase).

### Open questions
- None.
