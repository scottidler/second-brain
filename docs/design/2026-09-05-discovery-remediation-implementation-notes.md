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

## Phase 9: Oracle data dir under `sb/` (R1)

### Design decisions
- `SearchError` is a `thiserror` enum returned as an `eyre::Report` source —
  `vault/src/search.rs:SearchError::LegacyOracleDb` — mirroring `FabricError`
  in `vault/src/fabric.rs`. Every `SearchIndex::open` caller keeps its
  `eyre::Result` signature; only the one caller that needs to branch on the
  specific cause (`sb doctor`) downcasts. No signature churn across the
  workspace for one typed case.
- The guard is the literal first statement of `SearchIndex::open`, before the
  `create_dir_all(parent)` — `vault/src/search.rs:SearchIndex::open`. A test
  asserts the destination dir still does not exist after a refused open, so
  the ordering is pinned, not just commented. Without it, merely *checking*
  would mint `~/.local/share/sb/oracle/` and runbook R1's `mv -T` would nest
  the legacy dir inside it.
- The remedy string is one const shared by both surfaces —
  `sb/src/cli/checks.rs:LEGACY_ORACLE_REMEDY`. The `data dir` Warn and the
  `vault` Error print the identical `mv -T` command, so an operator reading
  either finds the same instruction.
- Doctor maps only the typed guard error to `Finding::error`; every other
  `SearchIndex::open` failure keeps its existing `Finding::warn(.., "sb oracle
  index")` — `sb/src/cli/checks.rs`. `sb oracle index` would not fix the
  legacy state, so pointing at it would be a wrong remedy.
- `cortex/src/classify.rs` keeps `.ok()` (Tier-2 similar-note context is
  optional) but gains `.inspect_err(|e| log::warn!(..))`, naming the DB path
  and stating that classify continues without Tier-2 context.
- `render_systemd_unit` resolves the data dir as `xdg_data_dir()/sb` with the
  same `.expect(...)` panic message the rest of `vault::paths` uses —
  `cortex/src/daemon.rs:render_systemd_unit`. Not a config field: the unit
  must grant exactly the namespace `vault::paths` resolves to, and a second
  knob could drift from it.

### Deviations
- Also updated `config/templates/oracle.yml.example:31`, the commented
  `logging.file:` example, from `~/.local/share/oracle/logs/oracle.log` to
  `~/.local/share/sb/oracle.log`. The phase scope named only line 6, but line
  31 pointed into a directory runbook step S1 deletes outright, and the real
  `oracle serve` log has been at `~/.local/share/sb/oracle.log` since Phase 7.
  Leaving a stale example in the same file being corrected for the same reason
  would have been a trap.
- Added a fourth guard test beyond the doc's three cases
  (`legacy_oracle_guard_leaves_other_paths_alone`): a non-oracle DB path opens
  normally even with the legacy DB present. Pins the `path == oracle_db_path()`
  half of the condition, which the three specified cases never exercise.
- Added `serial_test` to `vault`'s dev-dependencies. The guard tests mutate
  `XDG_DATA_HOME`, which is process-global; `vault` had no serialization
  primitive for env mutation (its existing `CWD_LOCK` covers CWD only).

### Tradeoffs
- Typed `SearchError` enum vs. string-matching the error text in doctor: the
  enum costs a new public type in `vault::search`, but the alternative is the
  exact anti-pattern `FabricError::is_timeout` was written to kill (a real
  error containing "legacy oracle" could masquerade as the guard).
- Fail-closed guard vs. auto-migration at open (doc Alternative 2): the guard
  makes a concurrent-opener race harmless instead of racing it. Cost is a
  deliberately broken host between the deploy and the operator move; a lost
  auto-migration race would cost the whole embedding corpus.
- Guard test asserts on the destination directory's absence rather than
  mocking `create_dir_all`: cheaper and it tests the real ordering, but it
  means the test depends on an `XDG_DATA_HOME` redirect and is Linux-gated
  (same limitation as the existing `cortex/src/sweep/tests.rs` precedent).

### Open questions
- **This host is now in the guarded state, pending runbook R1.** Nothing under
  `~/.local/share/oracle` or `~/.local/share/sb/oracle` was touched by this
  phase, by design. `~/.local/share/oracle/oracle.db` still exists (1.1 GB),
  `~/.local/share/sb/oracle` does not, and the freshly built binary's `sb
  doctor` reports (verbatim):
  `❌ [vault] legacy oracle DB at /home/saidler/.local/share/oracle but the current path is /home/saidler/.local/share/sb/oracle/oracle.db; refusing to create an empty index (runbook R1 moves it)`
  plus the matching `data dir` Warn. That is the guard working as specified.
  Verified after the run that `~/.local/share/sb/oracle` still does not exist.
  The installed `~/.cargo/bin/sb` is still the pre-Phase-9 binary (no `bump`,
  no `otto deploy` in this phase), so the live cortex daemon is unaffected
  until deploy. Runbook R1 (stop cortex, confirm no `sb oracle serve` process,
  `mv -T`) is Scott's to run by hand.
- The fourth success criterion — `sb doctor` shows the recorded note count
  after the move + deploy — is DEFERRED-TO-RUNBOOK. It cannot be checked
  before R1 runs.

## Phase 10: `sb bootstrap --prune-legacy-config` (S4)

### Design decisions
- `prune_legacy(apply: bool) -> Report` lives in `sb/src/cli/bootstrap/migrate.rs`
  beside `migrate_legacy_layout`, reusing the same `Report { lines, had_conflicts }`
  type: `had_conflicts` doubles as "had refusal(s)" for this call path, so
  callers don't need a second report shape.
- Fail-closed per directory, two independent gates before any delete is even
  considered: (1) `.migrated-to-sb` marker present (proof `migrate_legacy_layout`
  ran against it), (2) every file recursively under the directory is one of
  the seven known basenames (`sb/src/cli/bootstrap/migrate.rs::KNOWN_BASENAMES`,
  taken verbatim from the `plans` array at `:74-100`), a `.md` file under a
  `patterns/` subdirectory, or the marker itself. Either gate failing refuses
  the *entire* directory (no partial delete) and the message names the
  stranger file(s) so the operator can look before acting.
- Deletion goes through `borg::rkvr::remove(std::slice::from_ref(&dir))`
  (`borg/src/rkvr.rs:22`, widened from `pub(crate)` to `pub`) passing the
  whole directory as a single path; `prune_legacy` itself never calls
  `remove_dir_all` — that call lives inside `rkvr::remove`'s own std-fallback
  branch, which only fires when the `rkvr` binary isn't on PATH.
- `--prune-legacy-config` is a standalone action on `BootstrapArgs`: when
  present, `bootstrap::run` prints the report and returns immediately,
  skipping systemd install, model prefetch, and extension steps — mirroring
  `sb cortex migrate` being its own subcommand rather than a step folded into
  a larger pipeline. `--apply` is dry-run-off, same polarity as
  `cortex migrate --apply`.

### Deviations
- None. The doc's shape (`prune_legacy(apply) -> Report`, `--prune-legacy-config`
  + `--apply` flags, fail-closed per dir, `borg::rkvr::remove`, never
  `remove_dir_all`) matched the codebase seams as specced.

### Tradeoffs
- Recursive stranger detection walks every file under each legacy dir (not
  just top-level) so a stray file nested inside, say, a legacy `patterns/`
  subdirectory's own subdirectory would still be caught; the cost is a small
  hand-rolled `list_files_recursive` (no `walkdir` dep in `sb`/`borg`) instead
  of pulling in the crate `vault::paths::dir_size` already depends on.
- `prune_legacy` never bails/errors on a refusal (unlike `migrate_legacy_layout`,
  which bails the whole `sb bootstrap` run on `had_conflicts`): a refusal here
  just skips that one directory and reports why, so `borg`/`second-brain`
  still get evaluated even if `cortex` were somehow contaminated. Chose
  per-directory independence over an all-or-nothing gate since the runbook
  step (S4) expects the common case (three clean dirs) to proceed without an
  operator having to intervene for an unrelated one.

### Open questions
- None. `sb bootstrap --prune-legacy-config` (dry-run, no `--apply`) was run
  on desk and returned exactly the three legacy dirs the doc's success
  criterion names; nothing under `~/.config/{borg,cortex,second-brain}` was
  deleted. Running `--apply` (S4 in the operator runbook) is Scott's to do,
  gated on this phase's code being deployed.

## Phase 11: `system` tag group (F6)
### Design decisions
- `config/canonical-tags.yml:110`: added `system: []` beside `diy: []`, matching
  the existing pattern for domains that have no canonical tags of their own yet
  (the grouping is documentation; `vault::canonical::CanonicalTagsFile::all_tags`
  flattens across groups, so the empty list changes no runtime behavior).
- `vault/src/canonical/tests.rs::test_canonical_tags_groups_match_domain` —
  parses the *repo* copy of the YAML via `include_str!("../../../config/canonical-tags.yml")`
  (not the deployed `~/.config/sb/` copy, which drifts on its own schedule),
  then asserts both directions: every group key parses as a `Domain` via
  `Domain::from_str` (`vault/src/schema.rs:78`), and every `Domain::all()`
  variant has a matching group key via `Domain::as_str()`. Bidirectional so
  neither a stray/misspelled group key nor a `Domain` variant added later
  without a group key can land silently.

### Deviations
- None. Implemented at the exact file:line the doc names.

### Tradeoffs
- `include_str!` of the repo path vs. reading `vault::paths::canonical_tags()`
  (the deployed `~/.config/sb/canonical-tags.yml`) at test time — chose the
  embedded/compile-time copy so the test is deterministic and sandboxed (no
  dependency on a machine's `~/.config/sb/` state, no ordering requirement
  against `sb bootstrap`), matching the doc's own phrasing ("parse the
  embedded `config/canonical-tags.yml`").

### Open questions
- None.

### Success criteria (observed)
1. `cargo test --workspace --features vault/vec canonical` — all `canonical::tests::*`
   pass, including the new `test_canonical_tags_groups_match_domain`
   (`vault` lib test binary: `test result: ok. 20 passed; 0 failed`).
2. Break-the-test check: renamed the `system` group key to `sytem` in
   `config/canonical-tags.yml`, reran
   `cargo test --workspace --features vault/vec test_canonical_tags_groups_match_domain`
   — FAILED as expected:
   `thread 'canonical::tests::test_canonical_tags_groups_match_domain' panicked
   at vault/src/canonical/tests.rs:222:50: group key 'sytem' is not a valid
   Domain variant`. Reverted the typo; the same test then passed again
   (`test result: ok. 1 passed; 0 failed`).
3. Built `target/debug/sb` (`cargo build -p sb --bin sb --features vault/vec`
   — the workspace has no top-level `vec` feature, `sb`/`vault` do, so the CI
   task's `--features vec` becomes `--features vault/vec` at the `cargo build`
   call site; `otto ci`'s own `cargo test --workspace --features vec` invocation
   is unaffected since workspace-level `--features vec` resolves the same way
   through feature unification). Ran that binary's `bootstrap --force` (refreshed
   `~/.config/sb/canonical-tags.yml` from the embedded copy; failed later at
   the systemd `daemon-reload` step with "Operation not permitted" connecting
   to the user D-Bus session — expected in this sandboxed environment, not
   related to this phase, and the canonical-tags refresh had already
   succeeded and was verified byte-identical to the repo copy via `diff`
   before that unrelated failure). Then ran `sb doctor`, which printed
   `✅ [shared config] canonical-tags.yml: matches binary`. `sb doctor` also
   printed `❌ [vault] legacy oracle DB at ... refusing to create an empty
   index (runbook R1 moves it)` — the expected Phase 9 fail-closed guard;
   left untouched per instructions.

## Phase 12: Schema docs rendered from `vault::schema` (F3)

### Design decisions
- `description(&self) -> &'static str` added to all five enums with an exhaustive
  `match` — `vault/src/schema.rs::{Domain,NoteType,Origin,Status,Method}::description` —
  so a new variant cannot ship undescribed. Text seeded from the vault's
  `system/schemas/*-values.md` tables; ten `NoteType` variants (entity, session,
  digest, review, image, pdf, reddit, audio, document, code) and four `Method`
  variants (harvest, signal, discord, ntfy) got fresh prose because the vault
  doc never listed them (it carried 15 of 25 note types).
- `NoteType::Entity`'s description states the hub contract verbatim: "carries no
  `domain`, `origin`, or `status`". Pinned by
  `vault/src/schema/tests.rs::entity_description_states_the_hub_contract` so the
  clause cannot be paraphrased away; cortex.yml's `entities/**` path-exempt
  (Phase 14) is the enforcing half of the same rule.
- Drift comparison neutralises exactly one field — `cortex/src/schema_docs.rs::matches_render`
  re-renders using the `generated-at` value the on-disk file already carries, then
  compares byte-for-byte. A naive bytes-vs-disk compare would report drift on every
  run because the timestamp always moves. Consequence: an unchanged file is never
  rewritten, so `generated-at` records the last real content change rather than the
  last time anyone ran the verb. A file with no `generated-at:` in its leading
  frontmatter (the hand-written originals) can never match, which is the right answer.
  `cortex/src/schema_docs.rs::disk_generated_at` only reads the leading frontmatter
  block, so a `generated-at:` mentioned in the body cannot spoof the comparison.
- The schema-doc drift check runs FIRST in `sb/src/cli/checks.rs::vault_findings`,
  before the oracle config load. That function had three early `return vec![...]`
  paths (the Phase 9 legacy-oracle-DB guard among them); on this host the guard
  fires, so a finding appended after them would never be reachable. The three
  early returns now push onto the accumulated `findings` and return it.
- `oracle::server::schema_info_payload` split out of the `#[tool]` method
  (`oracle/src/server.rs`) so the `{value, description}` shape is unit-testable
  without an MCP server or an open search index — which matters on a host where
  the Phase 9 guard refuses to open the index at all.
- Snapshot fixtures live at `cortex/src/schema_docs/fixtures/*.md` with an
  `#[ignore]` regen test, mirroring `cortex/src/sweep/fixtures/cold-notes-expected.md`
  and `sweep/tests.rs::regenerate_cold_report_snapshot`.
- `cortex/AGENTS.md` Module Map gained a `schema_docs.rs` entry: Phase 2's
  `bin/agents-map` lint fails `otto ci` on any undocumented module (it did, and
  this is the fix).

### Deviations
- Design doc names the frontmatter keys as `type`/`domain`/`origin`/`generated-at`/
  `generator`/`pinned`. The renderer also emits `title`, `date`, and `tags: [obsidian]`.
  Reason: cortex.yml's `frontmatter.required` is `[title, date, type, domain, origin, tags]`.
  `system/**` is lint-excluded today, so nothing forces it, but shipping a
  policy-violating file from a generator is the wrong default. `date` is the date
  half of `generated-at`, from the same input, so the two can never disagree.
- `origin-values.md`'s "## The Key Distinction" prose is preserved as a second
  `const` (`ORIGIN_KEY_DISTINCTION`) rendered between the table and the Rules block,
  via an `extra: Option<&'static str>` field on `DocSpec`. The spec described one
  Rules const per file; folding this section into the Rules const produced a
  nested "Rules:" inside "## Rules". Same content, correct structure.
- The `origin-values.md` table loses its four-column shape (Value / Who decided it
  exists? / Who did the thinking? / Example) and `type-values.md` loses its "Typical
  origin" column, because the spec's contract is "the values table from the enum
  (value, description)". The dropped columns' content is folded into the
  `Origin::description` strings; the type doc's typical-origin hints are gone (they
  were advisory and partly wrong — `entity`, `digest`, `session` had no row at all).
- Two Rules lines were rewritten rather than carried verbatim, because a generated
  file cannot instruct the reader to edit it: domain's "New domain values require
  updating this file, the obsidian-borg classifier, and any Bases views" ->
  "added to `Domain` in `vault/src/schema.rs`; this file and any Bases views follow
  from there", and type's "any Dataview queries" -> "any Bases views" (the vault
  moved off Dataview). The stale "Values are single lowercase words. No hyphens,
  no underscores." line is dropped as instructed, pinned by
  `rules_blocks_drop_the_stale_no_hyphens_rule`.
- `oracle/src/server/tests.rs::schema_info_includes_session_note_type` pinned the
  OLD bare-string shape (`note_types.iter().any(|v| v == "session")`) and failed
  once the payload became objects. Inverted in place to assert
  `v["value"] == "session"` with a non-empty `description`, with the doc comment
  updated to say why — not deleted, not left green by accident.
- Criterion 2 was demonstrated against a temp copy of `system/schemas/` rather than
  the live vault, so the vault worktree was never touched at all (no render-then-
  restore). Rendering into the real vault is Phase 15's F3 step.

### Tradeoffs
- Neutralising `generated-at` in the compare, vs. dropping the field entirely: keeping
  it costs the special-case in `matches_render` but preserves a real "when did this
  last change" signal in the file. Dropping it would have made the compare trivially
  byte-exact and lost that.
- Rewrite-on-drift only, vs. always rewrite under `--render`: only-on-drift keeps the
  vault (Syncthing'd, git-tracked) free of no-op commits from a timestamp bump. Cost
  is that `--render` reports `unchanged` rather than `written` for a file that is
  already correct, which reads slightly oddly for a verb named "render".
- `Method` gets a `description()` with no corresponding `*-values.md` file. Skipping
  it would have kept the enum surface smaller; including it makes `schema_info`
  uniform across all five enums, which is the tool's whole point.

### Open questions
- Root `CLAUDE.md` does not mention `sb cortex schema` or that four of the five
  schema docs are now generated. Phase 10's scope explicitly included a CLAUDE.md
  edit; Phase 12's did not, so it was left alone. Worth a line in a later phase or
  the finalization commit.
- The four rendered files will land in the vault in Phase 15, replacing hand-written
  files whose `date: 2026-03-17` and `origin: authored` become `date: <render day>`
  and `origin: generated`. That is intended, but it is a visible metadata change on
  four long-lived notes.

### Observed output
- `otto ci`: green (`✅ All CI checks passed!`). One intermediate failure fixed
  inline: `agents-map` reported `FAIL: cortex/AGENTS.md: schema_docs.rs undocumented`,
  plus `cargo fmt` diffs in the three new test blocks.
- Criterion 1 — `cargo test --workspace --features vec schema_docs`:
  `test result: ok. 10 passed; 0 failed; 1 ignored` (the ignored one is the
  fixture-regeneration test).
- Criterion 2 — against a temp copy of the vault's `system/schemas/`
  (`diff -r` against the real directory: IDENTICAL before the run):
  `sb cortex --vault <tmp> schema --check` printed four `drifted` lines and
  `exit=1`; `--render` printed four `written` lines and `exit=0`; a second
  `--check` printed four `unchanged` lines and `exit=0`; a bare
  `sb cortex schema` (no flag) also printed `unchanged` and `exit=0`, confirming
  `--check` is the default. `frontmatter.md` in the temp copy stayed byte-identical
  to the vault's. `git -C ~/repos/scottidler/obsidian status --porcelain system/schemas/`
  is empty: the vault worktree was never modified.
- Criterion 3 — `sb oracle call schema_info` could NOT be run on this host: the
  Phase 9 fail-closed guard refuses, exactly as designed
  (`Error: Failed to open database / legacy oracle DB at ~/.local/share/oracle but
  the current path is ~/.local/share/sb/oracle/oracle.db; refusing to create an
  empty index (runbook R1 moves it)`, exit 1). Proved instead by
  `oracle/src/server/tests.rs::schema_info_payload_emits_value_description_pairs`,
  which asserts every one of the five keys (`domains`, `note_types`, `origins`,
  `statuses`, `methods`) is an array of `{value, description}` objects with
  non-empty descriptions — 53 description fields in total, well over the
  criterion's threshold of 5. `cargo test --workspace --features vec schema_info`:
  `test result: ok. 2 passed; 0 failed`.
- Doctor, on this host (Phase 9 guard live), prints BOTH findings in the `vault`
  section, which is the point of moving the schema check ahead of the early returns:
  `❌ [vault] legacy oracle DB at /home/saidler/.local/share/oracle ... (runbook R1 moves it)`
  then
  `⚠️  [vault] system/schemas/*-values.md drifted from binary (system/schemas/domain-values.md, system/schemas/type-values.md, system/schemas/origin-values.md, system/schemas/status-values.md)`
  with `-> sb cortex schema --render`. Before the reorder the Warn would have been
  unreachable behind the guard's `return`.

## Phase 13: PR-time CI (R4)
### Design decisions
- New `.github/workflows/ci.yml`, one job (`ci`), mirrors `release.yml:17-54`
  exactly for the container/apt/rustup/cache shape (`RUST_VERSION: 1.96.0`,
  `debian:bookworm`, `Swatinem/rust-cache@v2` with `shared-key: ci` instead of
  the release workflow's per-target key, since this workflow builds one
  target-less job) — `.github/workflows/ci.yml`.
- Step order follows the assignment's literal ordering (fmt, check, clippy,
  test, bloat, agents-map) rather than `.otto.yml`'s internal `check` task
  order (check, clippy, fmt); all four are independent checks so order has no
  behavioral effect, and fmt-first surfaces the cheapest failure fastest in CI.
- `bloat` and `agents-map` steps inline/call the same logic as their `.otto.yml`
  tasks rather than invoking `otto` itself in CI (otto is not installed in the
  `debian:bookworm` container and installing it was out of scope for this
  phase) — `bloat` is the verbatim shell body from `.otto.yml:24-48`;
  `agents-map` invokes `bin/agents-map` by path, the same script `.otto.yml:53`
  calls, so there is exactly one implementation as the doc requires.
- `on: push: branches: [main]` plus bare `pull_request` (all PR types, matching
  the doc's "PR-time CI" framing; no path filters, since every crate in the
  workspace can affect any other via the shared `vault` lib).
- Added `rustup component add clippy rustfmt` (not present in `release.yml`,
  which never runs clippy/fmt) since this workflow's step list requires both.

### Deviations
- None from the phase's scope. `release.yml`'s per-matrix-target build/package/
  release steps are correctly absent here; this workflow is check-only, not a
  release build.

### Tradeoffs
- Inlining `bloat`'s shell body vs. installing `otto` in the container: chose
  inlining because the doc explicitly says "the `bloat` loop inlined," and
  installing a second binary (otto) into the CI container was not itself in
  the addition list under Architecture.

### Open questions
- None.

### Local validation (both push-gated success criteria are DEFERRED-TO-PUSH)
- `yl .github/workflows/ci.yml`: 3 line-length errors (lines over 80 chars).
  The already-committed `.github/workflows/release.yml` fails the identical
  check with the same count (3 long lines) under the same linter with no
  project-level `yl` config present, so this is pre-existing, unenforced
  style in this repo, not a defect introduced here.
- `python3 -c "import yaml; yaml.safe_load(open(...))"`: parses with no error.
- Step list vs. `.otto.yml` `check`/`test` tasks: `cargo fmt --all --check`,
  `cargo check --workspace --all-targets --features vec`,
  `cargo clippy --workspace --all-targets --features vec -- -D warnings`,
  `cargo test --workspace --features vec` all textually identical to
  `.otto.yml:63-76`. `bloat` step body textually identical to `.otto.yml:25-48`
  (including `BLOAT_MAX_LINES` default and the same `find` exclusions).
  `agents-map` step calls `bin/agents-map`, the same invocation as
  `.otto.yml:53`. `lint` (`whitespace -r`) intentionally omitted per the
  assignment (Scott's own binary, not installable in CI).
- `bin/agents-map` confirmed executable (`-rwxrwxr-x`) and invoked by relative
  path from repo root, matching `.otto.yml:53`'s invocation.
- `otto ci` run locally: exit 0, all of `lint, bloat, agents-map, check, test`
  passed (`ci` finished successfully; 410 passed in vault's lib tests,
  `candle_bert_matches_sentence_transformers_reference` ran and passed
  (offline, fixture-based), `candle_bert_rss_plateaus_across_1000_calls` and
  `perf_scan_vault_thousand_notes` both `ignored`).
- Offline-test-path claim verified by direct inspection, not just by running
  the suite once:
  - `vault/tests/regression/candle/parity.rs`: reads
    `tests/fixtures/bge-reference.json`; on read failure it `eprintln!`s a hint
    and returns (skip, not fail) rather than downloading anything. Confirmed
    `git ls-files vault/tests/fixtures/` is empty in this worktree, i.e. the
    fixture is not committed, so this test skips in a fresh CI checkout by
    construction (it happened to pass locally because a previously-generated
    fixture file sits untracked in this dev tree).
  - `vault/src/embedding/candle/tests.rs`: the real-model parity test reads
    `CANDLE_TESTS_REAL` and returns early (skip) unless it equals `"1"`; CI
    workflow sets no such env var, so it is always skipped in CI.
  - `vault/tests/candle-bounded.rs` and `vault/tests/perf.rs`: both carry
    `#[ignore]` (confirmed by grep), so `cargo test` does not run them by
    default; the workflow issues plain `cargo test`, no `-- --ignored`.
  - `cortex/tests/hub_retrieval_contract.rs`: loads
    `tests/fixtures/bge-small-en-v1.5-tokenizer.json` via `Tokenizer::from_file`
    (a real, committed ~711 KB file per its own doc comment, unlike the candle
    fixture above) and never calls out to a model-weights download; tokenizing
    needs no inference. Confirmed no `download`/`network`/hf-hub calls in the
    file.
  - Net: the default `cargo test --workspace --features vec` path in this CI
    workflow makes zero network calls.
- Both push-gated success criteria (`gh workflow view ci` lists the workflow;
  first run on `main` is green, warm-cache duration recorded) are
  DEFERRED-TO-PUSH per the assignment; not attempted in this phase.

## Phase 14: dotfiles (F4, F2 config, S2 config, `Inbox/**` casing)

### Design decisions
- Repo: `~/repos/scottidler/dotfiles`, files `HOME/.config/sb/{borg,cortex}.yml`
  (symlinked live into `~/.config/sb/`; confirmed with `diff` against the
  `~/.config/sb/*` symlink targets before and after editing — identical, so the
  edit is live for the running cortex daemon on its next 300 s tick with no
  restart).
- `cortex.yml:20-25`: deleted the `schema:` block entirely. `Config` is
  `#[serde(default)]` at struct level (`cortex/src/config.rs:8`), so the
  missing key falls back to `SchemaConfig::default()` (enum-derived).
- `cortex.yml:39`: `"Inbox/**"` -> `"inbox/**"` (capital I never matched
  `inbox/`).
- `cortex.yml actions.frontmatter.path-exempt`: added `"entities/**": [domain,
  origin]`.
- `cortex.yml vault.ignore`: added `.claude` and `templates`.
- `cortex.yml migrations`: added the `v4-legacy-values` entry verbatim after
  `v3-domain-expansion`, per the doc's literal YAML block.
- `borg.yml:121`: `log-level: debug` -> `log-level: info`.
- Staging: `borg.yml` carried an unrelated, pre-existing, uncommitted hunk (a
  `fabric:` block rewrite to a bare `binary: fabric` PATH-resolved name plus a
  `max-tokens: 16384` addition) already dirty in the working tree before this
  phase touched the file. Used `git add -p` to stage only the `log-level`
  hunk, leaving the fabric-block hunk unstaged so it does not ride along in
  this phase's commit. `cortex.yml`'s diff contained only this phase's five
  edits, so it was staged whole.

### Deviations
- None. Every edit matches the doc's line numbers and content exactly
  (`cortex.yml:20-25`, `:39`, `path-exempt`, `vault.ignore`, `migrations:`
  after `v3-domain-expansion`; `borg.yml:121`).

### Tradeoffs
- None.

### Open questions
- None.

### Success criteria (verified against the freshly built
  `second-brain/main/target/debug/sb`, not the stale `~/.cargo/bin/sb`)
- `sb doctor 2>/dev/null | grep '\[config\]'`: PASS — all three lines print
  `✅ [config] {borg,cortex,oracle}: ... (parses as typed Config)`.
- `sb cortex lint 2>/dev/null | grep -c 'not valid'`: PASS — 89 (doc's expected
  value; was 2016 before this phase per Phase 7's "Observed on main" note).
- `grep -c '\[frontmatter.required.domain\]'` = 2 (<= 10, PASS); `grep -c
  '\[frontmatter.required.origin\]'` = 14 (<= 30, PASS); `grep -c '\.claude/'`
  = 0 (PASS, was 2).
  - "`sb doctor` shows the same two numbers with no rebuild": UNVERIFIED. Root
    cause: `sb/src/cli/checks.rs::vault_findings` (`:731-785`) opens the oracle
    `SearchIndex` before calling `frontmatter_policy_findings` (`:784`), and
    Phase 9's fail-closed legacy-oracle guard (`SearchIndex::open`) hits an
    `Err` on this host (the legacy DB at `~/.local/share/oracle` still exists;
    the new `~/.local/share/sb/oracle` path does not) — `vault_findings`
    early-returns at that `Err` arm (`:757-760`) and never reaches the
    frontmatter-gaps `Info` line. This is the same pre-existing, intentional
    Phase 9 condition the assignment named ("cleared by operator runbook R1
    which Scott runs by hand"); per explicit instruction I did not touch
    `~/.local/share/oracle` or `~/.local/share/sb/oracle` to clear it. The
    config-driven policy itself is proven by the lint numbers above (no
    rebuild between the pre-edit 920/931 and post-edit 2/14); only doctor's
    independent-path echo of those same numbers is blocked by the unrelated
    R1 prerequisite.
- `sb cortex migrate 2>/dev/null` (dry run, no `--apply`): PASS — `grep -c
  "would rename origin: 'human'"` = 87, `grep -c "would rename origin:
  'ai-generated'"` = 1, `grep -c "would rename status: 'resolved-workaround'"`
  = 1. All three match the doc's expected counts exactly.

## Phase 15: Vault repo edits

### Design decisions
- F9's new `work/` row was placed between `entities/` and `system/` in both
  `CLAUDE.md` and `README.md` — keeps content dirs grouped together with the
  operational `system/` row last, matching the existing (non-alphabetical)
  ordering already in both tables.
- README.md's `work/` table row was written unquoted (`| work/ |`, not
  `` | `work/` | ``) even though every other README row backtick-quotes the
  folder name — the phase's own success criterion (`grep -c '^| work/'
  CLAUDE.md README.md` = 1 each) pins the literal unquoted line start, so the
  one row breaks the table's quoting convention to satisfy the stated check.
- `frontmatter.md`'s new "entity hubs carry no domain/origin/status" line
  (Phase 12's deferred hand edit) was written as plain prose with no wikilink
  — the doc's quoted text is bare prose; an invented `[[domain-values]]`
  cross-reference would have pointed at a heading that render_all doesn't
  create.
- `daily.md`'s pushups/situps became the literal `20` the doc specifies
  (not the old `workWeekNum + 10` formula's typical output) — doc's F7 bullet
  states the number outright, so no attempt was made to compute an
  equivalent starting value.

### Deviations
- **S7 (reingest) could not complete: the source YouTube video is gone.**
  `sb borg reingest --source https://www.youtube.com/shorts/iDISCSQn6mI`
  returned `Failed: fetch-failed`. Root cause confirmed independently of the
  reingest path: a direct `yt-dlp --skip-download --simulate` against the
  same URL returns `ERROR: [youtube] iDISCSQn6mI: This video is unavailable`
  — YouTube's own API says the video no longer exists (this is not a sandbox
  or network-egress artifact; the request reached YouTube and got a real
  answer). `sb borg log --trace <trace>` confirms `status: failed`,
  `failure_stage: fetch-failed`. Because the pipeline never reaches the
  publish step, `notes/prompt-caching-cuts-claude-code-bills-by-80.md` is
  byte-identical to its pre-phase state: the `## Transcript` section is still
  present (count 1, not the doc's target 0), and the file produced no `git
  diff`, so it was not staged (nothing to stage — content matches HEAD).
  This is the one bullet of Phase 15 left undone; it requires either the
  video coming back or Scott choosing a different remediation for that one
  note (a different source, or one of the rejected alternatives such as
  hand-stripping the transcript).
- **Shell `grep` gave an intermittently wrong (0) count for the literal
  string `entities/` in `README.md`**, even immediately after independent
  confirmation (via `sed -n '13p'`, `od -c`, and Python's `str.count`/
  `re.findall`) that the exact bytes `entities/` are present exactly once,
  with no non-ASCII characters. The same `grep -c entities` call also failed
  against a byte-identical copy of the file in the scratch directory on a
  later invocation despite succeeding on an earlier one — non-deterministic,
  not a content problem. Verified via Python throughout instead; every other
  grep-based criterion in this phase reproduced correctly across repeated
  calls. Recorded here as a real, unexplained tooling anomaly rather than
  silently working around it.

### Tradeoffs
- None beyond what's recorded above.

### Open questions
- S7's note (`notes/prompt-caching-cuts-claude-code-bills-by-80.md`) still
  carries its `## Transcript` section because the source video is gone from
  YouTube. Scott's call: leave the note as-is (it already has a transcript,
  just not a freshly regenerated one), or pick a different remediation path
  for this one note.
- The `sleep 660` two-tick re-stamp check was started in the background;
  first reading (immediately after the templates rewrite) is `cortex- count
  = 0`. Second reading (after ~11 minutes / two daemon ticks) will be
  reported separately once the background wait completes — not blocking this
  writeup.
