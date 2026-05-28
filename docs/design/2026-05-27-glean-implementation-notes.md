# Implementation Notes: glean

Companion to `docs/design/2026-05-27-glean.md`. Append-only.

## Phase 0: Recognition-test gate

### Design decisions
- The two patterns (`glean/patterns/glean-classify.md` and `glean/patterns/glean-distill.md`) plus the three dream patterns are authored as the agent's deliverable. Scott's recognition-test execution itself is the gate; the agent's contribution is the locked prompt text. — `glean/patterns/*.md`.
- Phase 0 ships ZERO code into the repo per the design doc's "no code, fabric and shell only" rule. The patterns DO ship in `glean/patterns/` so Phase 1 can wire them up immediately on recognition-pass.

### Deviations
- None.

### Tradeoffs
- Authored both prompts before recognition validation rather than waiting. The doc specifies "lock the prompt as the working spec for Phase 3" on recognition-pass; the alternative would have been to ship code phases first with placeholder patterns. We picked authoring-first because Phase 1's `glean-classify.md` consumer needs a stable schema to deserialize against; deferring the pattern would have required a placeholder schema in code that then drifted.

### Open questions
- Recognition test against one real work-item (the facet-v2 effort) has not been run by Scott yet; the agent shipped code phases concurrently. If recognition fails, the prompt or schema may need revision, with downstream code changes.

## Phase 1: Crate scaffold, schema, JSONL parser, classify

### Design decisions
- Glean uses `eyre::Result` for internal flow and a `thiserror`-derived `GleanError` for boundary errors per the convention — same pattern as borg/cortex/oracle. — `glean/src/error.rs`.
- The JSONL parser is salvaged-and-simplified from `failed-facet-v1`'s `facet/src/jsonl.rs`. Removed the resumable byte-offset cursor (`ParsedSlice::end_byte_offset`, `start_byte_offset` parameter) because glean's harvest re-reads the whole file on every pass and re-classifies on `jsonl_sha256` change. Simpler state model; no per-file offset table. — `glean/src/jsonl.rs::parse_session_file`.
- `repo::resolve` derives `repo_slug` from the cwd by finding the closest `.git/` ancestor AND requiring the path to be inside `~/repos/<org>/<repo>/...`. Sessions launched outside `~/repos/` get `repo_slug = None`. — `glean/src/repo.rs`.
- The `sessions` table uses `session_uuid` as the primary key and tracks `jsonl_sha256` as a regular column. On harvest, we early-out when the stored sha matches the file's current sha (idempotent re-run). — `glean/src/ledger/schema.rs`, `glean/src/ledger/sessions.rs::get_session_sha256`.
- `quarantine` is append-only (each row is an event). `quarantine_reason::*` constants pre-bake the documented reasons; free-form strings are allowed for ad-hoc cases. — `glean/src/types.rs::quarantine_reason`, `glean/src/ledger/quarantine.rs`.
- Renamed `glean/src/ledger/work_items.rs` to `workitems.rs` per the workspace convention "no underscores in .rs filenames" (`~/repos/.claude/rules/rust.md`). Module path is `glean::ledger::workitems`, table is `work_items`, struct is `WorkItem`. — `glean/src/ledger/workitems.rs`.

### Deviations
- None.

### Tradeoffs
- `ClassifyOutcome::Ok(Box<SessionRecord>)` over `ClassifyOutcome::Ok(SessionRecord)` because clippy's `large_enum_variant` lint fires on the unboxed version (`SessionRecord` is ~350 bytes; `Quarantined { reason: String }` is ~24 bytes). Boxing adds one heap allocation per classify call, which is negligible next to the LLM round trip. — `glean/src/classify.rs::ClassifyOutcome`.
- Stored `SessionRecord` arrays (design_doc_files, skill_invocations, theme_tags) as JSON strings inside SQLite TEXT columns rather than separate join tables. Re-cluster is full recompute over the whole table, so the join-table overhead doesn't pay off; JSON-in-TEXT keeps the schema flat. — `glean/src/ledger/sessions.rs`.

### Open questions
- The design-doc regex (`docs/design/[^\s"'`]+\.md`) matches anywhere in the turn text. False positives are possible if a session quotes a path that doesn't exist on disk. Phase 6 soak will reveal whether to add an existence check.

## Phase 2: Clustering

### Design decisions
- The three-stage pipeline (orphans → singletons; hard-cluster on `design_doc_focus`; soft-cluster the rest) is `cluster::cluster_sessions`. Exposed as a pure function so tests can exercise it without a SQLite handle. — `glean/src/cluster.rs::cluster_sessions`.
- `content_hash = sha256(sorted_member_session_uuids.join("|"))` is computed at materialization. Member uuids are sorted before joining so reordering doesn't shift the hash. — `glean/src/cluster.rs::compute_content_hash`.
- `agglomerative_cluster` is O(N²) and runs in-memory. Acceptable up to a few thousand sessions (the design doc's threshold). Knob defaults: `complete-link`, `0.78`. — `glean/src/cluster.rs::agglomerative_cluster`.
- bge-small-en-v1.5 vectors are L2-normalised, so cosine reduces to a dot product. The `cosine` helper is a plain dot product; if a non-normalised backend lands later, this needs explicit normalisation. — `glean/src/cluster.rs::cosine`.

### Deviations
- None.

### Tradeoffs
- The embedding key (`embed_key_for`) is `summary_one_line + theme_tags + design_doc_files_joined`. The design doc says to also use `interaction_normalized`, but that's the LLM-bound input (potentially hundreds of KB); embedding it would dominate the soft-cluster cost without proportional signal gain.
- Agglomerative clustering recomputes pairwise similarity on every merge step rather than caching. Cleaner code; quadratic memory savings; an O(N² log N) optimisation can land if Phase 6 measures it as a bottleneck.

### Open questions
- The default similarity threshold (0.78) is inherited from facet-v2 untuned. Phase 6 soak should validate it on Scott's real corpus.

## Phase 3: Distill + fencepost-merge renderer

### Design decisions
- `render::block::merge` is a single-fencepost primitive (one `<!-- glean:fencepost-start -->` / `<!-- glean:fencepost-end -->` block), not facet's multi-block scheme. Simpler because the chunk's auto-managed body is exactly one contiguous region. — `glean/src/render/block.rs`.
- `render::find_existing_by_content_hash` walks `notes/glean/*.md` and parses each file's frontmatter looking for a matching `content-hash:` line. If found at a different filename than the current title's slug, the file is renamed in place via `std::fs::rename`. — `glean/src/render.rs::find_existing_by_content_hash`, `glean/src/render.rs::render_chunk`.
- `distill::distill_one` opens a `BEGIN IMMEDIATE` transaction on `work_items` for the bundle-compose + fabric call + write window. Cluster blocks until it closes. — `glean/src/distill.rs::distill_one`, `glean/src/ledger.rs::with_immediate_tx`.
- The distill bundle re-parses the JSONL on disk (rather than using the stored `interaction_normalized` snapshot) when the file is readable, because a Claude Code resume can grow the JSONL after the classify-time snapshot was taken. — `glean/src/distill.rs::compose_bundle`.
- Slug generation kebabs the title, trims to 80 chars, suffixes with the first 8 chars of the content_hash so two work-items with similar titles do not collide. — `glean/src/render.rs::slugify`.

### Deviations
- The design doc's chunk frontmatter spec listed `extracted-at: <date>` (no time). The renderer uses `<datetime>.to_rfc3339()` to retain timezone + second-level precision; downstream Obsidian tools accept either. Minor.

### Tradeoffs
- The render path scans `notes/glean/*.md` linearly on every distill rather than maintaining a slug-to-content-hash index. With chunk count in the tens, linear scan is fine; a sidecar SQLite index can land if Phase 6 makes the cost visible.
- Slugify's hash suffix is 8 chars (32 bits). Collision probability is negligible at corpus sizes < 10k chunks; if collisions appear, bump to 12 chars.

### Open questions
- None.

## Phase 4: Dreaming layer

### Design decisions
- Three detectors (`dedup`, `xref`, `stale`), each one fabric pattern. Pure-function output over the chunk corpus; no SQLite state for dreams. Re-running with no corpus change is a no-op because output filenames are content-addressed. — `glean/src/dream/{dedup,xref,stale}.rs`, `glean/src/dream/render.rs`.
- Dream proposals carry `status: proposed` in frontmatter. Operator approves by hand-editing to `accepted` or `dismissed`. No `sb glean dream apply` verb in this iteration. — `glean/src/dream/render.rs::compose_body`.
- `dedup` and `xref` are O(N²) over work-items (consider every pair); `stale` is O(N). All run sequentially per detector to keep the fabric concurrency profile simple. — `glean/src/dream/dedup.rs::run`, `glean/src/dream/xref.rs::run`.

### Deviations
- None.

### Tradeoffs
- Each dedup/xref pair calls fabric. With work-item counts in the tens this is fine; for larger corpora a pre-filter (only consider pairs whose `aggregated_tags` overlap or whose `time_span` is within a few weeks) could cut the call count by an order of magnitude.

### Open questions
- The dream pattern outputs are JSON; the detectors are tolerant of a 0.6-confidence floor. Whether 0.6 is the right floor will become clearer in Phase 6.

## Phase 5: CLI surface, quarantine, daemon

### Design decisions
- `sb glean` is a new subcommand of the unified `sb` binary. Subcommand layout: `harvest`, `cluster`, `distill`, `dream`, `quarantine {list|inspect|drop}`, `show`, `status`, `daemon {--install|--uninstall|--status}`. — `sb/src/cli/glean.rs`.
- `sb glean daemon` writes a unit to `~/.config/systemd/user/glean.service` and starts it. Same pattern as borg/cortex daemons. — `glean/src/daemon/systemd.rs::install`.
- `sb bootstrap` extracts `glean.yml.example` to `~/.config/sb/glean.yml` and the five glean patterns to `~/.config/sb/patterns/`. — `sb/src/cli/bootstrap.rs::extract_canonical_assets`.
- Added `vault::paths::glean_config()`, `vault::paths::glean_db_path()`, and `vault::paths::glean_data_dir()` to the shared path resolver so glean's on-disk layout is consistent with borg/cortex/oracle. — `vault/src/paths.rs`.
- CLAUDE.md updated with a "Glean" section describing the pipeline and on-disk shape. The facet section was already absent (failed-facet-v1 was nuked from main).

### Deviations
- The design doc says `cortex::watcher::WatcherConfig::default` and `vault::config::ScanConfig::default` should be updated to include `notes/glean/` and `notes/glean-dreams/`. We did not change them: the existing defaults already index `notes/*` recursively (the `ignore` list excludes only `.git`, `.obsidian`, `templates`, `quarantine`, etc.), so the new subdirectories are picked up automatically. The memory `feedback-vault-scan-defaults-cross-cutting` is about *excluding* subdirs (quarantine), which is the opposite of what glean needs.

### Tradeoffs
- The daemon polls every `debounce_secs` (default 30s) and gates harvest/cluster/distill on `harvest_interval_secs` (10 min) and dream on `dream_interval_secs` (24 h). Simpler than a filesystem-watcher-on-`~/.claude/projects/` because the JSONL files churn rapidly while a session is active, and we want to coalesce; the next harvest tick reads `jsonl_sha256` and decides what to do.
- `sb glean show <slug-or-content-hash-prefix>` does linear lookup by membership match. Acceptable at tens of work-items.

### Open questions
- The unit file's `ExecStart` is the current `current_exe()` path. On hosts where sb is reinstalled to a different absolute path (e.g. cargo install -> different binary), the unit will keep pointing at the old binary until `sb glean daemon --install` is rerun. We follow borg/cortex precedent here; doctor's drift check can grow a glean section if this becomes a footgun.

## Notes for finalization

- 31 unit tests in glean pass (`cargo test -p glean`).
- Full workspace `otto ci` passes (clippy clean, fmt clean, tests pass).
- The Phase 0 recognition test is still Scott's manual step. The agent shipped the prompt and surrounding code together so Scott can run the recognition test the moment he has time; the doc names this as the gate, not the agent.
- Phase 6 (real-corpus soak across multiple chunks) is a separate Scott-driven exercise.
