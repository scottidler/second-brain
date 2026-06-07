# Implementation Notes: Cold-Sweep Content-Date Clock

Running record of how the implementation interprets or diverges from
`2026-06-07-cold-sweep-content-date-clock.md`. Append-only.

## Phase 4: Frontmatter scalar coercion (landed first)

Landed first as a standalone, individually-green commit (`f049ef8`) since the
design notes it has no ordering dependency on Phases 1-3 and `normalize_date`
is robust to both old and new parser output.

### Design decisions
- Tests placed in the existing `#[cfg(test)] mod tests` block in
  `vault/src/frontmatter.rs` rather than extracting to `frontmatter/tests.rs`.
  The design's Phase 4 explicitly permits "the existing test module"; extracting
  the whole module is an unrelated mechanical refactor and out of scope here.
- `scalar_to_string` placed near the top of the file (after the imports) as a
  free fn — `Frontmatter::from_value` is the only caller.

### Deviations
- None.

### Tradeoffs
- Kept the eight call sites as `field = scalar_to_string(val);` one-liners
  rather than collapsing the eight match arms into a table-driven loop. The
  explicit arms match the existing style and keep `tags`/`pinned` visibly
  special-cased.

### Open questions
- None.

## Phase 1: Indexer normalization + query + types

### Design decisions
- `normalize_date` placed alongside the other module-level helpers in
  `vault/src/search.rs` (after `normalize_enum`), matching the spec.
- Per the logging rule and advisor, `normalize_date` is a trivial pure helper
  and carries no entry log; the `cold_notes` debug line was switched from
  `q.older_than` to `q.before_date`.

### Deviations
- The design's Architecture section lists only the four query/type/renderer
  touchpoints. I additionally corrected two now-false code comments in
  `search.rs` (the `idx_notes_modified_at` justification and the migration-path
  comment) that claimed "the cold-note SELECT filters on `modified_at`" — it no
  longer does. Pure comment fixes; no behavior change. `idx_notes_modified_at`
  is retained per the Non-Goals (other code reads `modified_at`).

### Tradeoffs
- `index_one`'s `date` binding became an owned `String` (was `&str`). The
  `params!` macro binds by reference and only one of the INSERT/UPDATE arms
  executes per call, so the single owned value is inert.

### Open questions
- None.

## Phase 2: Cortex wiring + renderer

### Design decisions
- `before_date` floor computed with `chrono::Utc::now().date_naive() -
  chrono::Duration::days(...)` exactly as the spec's API Design snippet. Cloned
  once into `ColdQuery` so the same string can also be passed by reference to
  `count_pinned_excluded` — the two predicates provably share the floor.
- Renderer drops the `from_timestamp` formatting entirely and emits `row.date`
  verbatim; per-row suffix is now `dated {date}` (Open Question resolved in the
  doc).

### Deviations
- None.

### Tradeoffs
- None.

### Open questions
- None.

## Phase 3: Tests + fixture

### Design decisions
- Added two test-only helpers in `vault/src/search/tests.rs`
  (`make_dated_note`, `make_dated_pinned_note`) rather than changing the shared
  `make_test_note`/`make_pinned_note` signatures. Per the advisor, only the cold
  tests need a content date; widening the shared helpers would ripple into the
  signal/pinned tests that don't care about date.
- Cold tests now seed ISO `date:` values and assert against a `"2024-01-01"`
  floor; the `index_one` mtime argument is kept (arbitrary `1_000`) since the
  `modified_at` column still exists, but it no longer influences coldness.

### Deviations
- The design's Phase 3 test list under-enumerated the breakage set. Beyond the
  three named `cold_notes_*` tests, I also updated, with old `date:`
  frontmatter, the three cortex integration tests the doc did not name:
  `cold_with_index_counts_pinned_excluded`, `cold_with_index_writes_report_atomically`,
  and `test_daemon_cold_tick_fires` (the last surfaced only at CI run, not from
  the static diagnostics). All four `make_cold_note` call sites and the
  `render_cold_report_groups_..._metadata` `"last modified"` assertion were
  updated to the new date-string shape / `"dated"` wording.
- Added `cold_notes_excludes_undated_rows` as its own test (the doc folded the
  empty-date case into one of the existing tests; a dedicated test reads
  clearer and the floor-satisfying test also carries an undated row).

### Tradeoffs
- None.

### Open questions
- None.
