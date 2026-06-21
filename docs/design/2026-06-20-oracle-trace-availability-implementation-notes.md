# Implementation Notes: Oracle trace (staged-source) availability

Running, append-only record of how the implementation interprets or diverges
from `2026-06-20-oracle-trace-availability.md`. One section per phase.

## Phase 1: vault schema + frontmatter

### Design decisions
- `Frontmatter.trace_expires` is the Rust field; the frontmatter/YAML key is
  `trace-expires` (hyphenated) — handled by an explicit match arm in
  `parse_frontmatter` and explicit emission in `to_yaml()`
  (`vault/src/frontmatter.rs`). Matches the global hyphen-key convention.
- Forced reindex implemented as `SearchIndex::index_vault_force(root, force)`
  with `index_vault` delegating `force=false` (`vault/src/search/index.rs`).
  This kept the four other `index_vault` call sites (serve, watcher, MCP
  reindex) untouched — only the CLI `index` path opts into force. The mtime
  gate became `if !force && existing_mtime == Some(mtime)`.
- The `clone_frontmatter` helper in `cortex/src/summarize.rs` is a second,
  in-code instance of the to_yaml data-loss trap (it rebuilds a Frontmatter
  field-by-field). Added the three keys there too, or `summarize --backfill`
  would strip them. Cited explicitly because the design only named the
  `to_yaml()` site.

### Deviations
- The migration-idempotency test exercises the ALTER path through the
  `pub(crate) ensure_schema()` (against a hand-built pre-trace `notes` table)
  rather than calling `ensure_trace_columns()` directly: the latter is private
  to the `schema` submodule and not reachable from the sibling `tests` subtree.
  Same coverage (real ALTER + idempotent second run), legal visibility.

### Tradeoffs
- Chose `index_vault_force` wrapper over adding a `force: bool` param to
  `index_vault` itself — fewer call-site edits, and the common name keeps the
  default (gated) behavior obvious. Alternative (bool param everywhere) was
  noisier for no benefit since only one caller needs force.

### Open questions
- The design's standing micro-question — whether the `--force` flag is even
  needed vs. letting normal mtime-driven reindex backfill over time — is left
  as shipped: the flag exists (resolved decision #7) but normal reindex still
  backfills the additive columns eventually. No user action required.

## Phase 2: oracle response surface

### Design decisions
- `trace_block` is a private associated fn on `OracleMcpServer`
  (`oracle/src/server.rs`), folded into the `metadata` JSON object so it rides
  at every `DetailLevel` (metadata and above) for free — no per-level edits.
- `within-window` uses `chrono::Utc::now().date_naive() <= expires` parsed with
  `%Y-%m-%d`, matching the ledger's UTC-calendar-date convention named in the
  design's Dependencies section.
- Key ordering in the block is insertion order via `serde_json::Map`
  (available, ref, ingested, expires, within-window) — `expires` is inserted
  only on a successful parse, so it is naturally omitted otherwise.

### Deviations
- None. Shapes match the API Design section exactly: `{available:false}` alone
  when no handle; `expires` omitted + `within-window:null` when expiry is
  absent or unparseable; a single `warn!` (never an error) on unparseable.

### Tradeoffs
- Emitted the block inside the base `metadata` json! macro rather than
  inserting it per-arm in the `match detail_level`. Less code, and it
  guarantees the block can never drift between levels.

### Open questions
- None.

## Phase 3: borg stamps trace-expires at publish

### Design decisions
- Added two pure helpers to `borg/src/retention.rs` (the retention owner):
  `parse_ingested_date` (accepts bare `%Y-%m-%d` AND RFC-3339 offset datetime)
  and `trace_expires_for(date, retention_days) -> %Y-%m-%d`. Single source of
  the expiry math, shared by Phase 3 publish and Phase 4 backfill.
- At publish (`borg/src/pipeline.rs`, the URL handler), the note's `ingested`
  instant IS `now` (written unconditionally at the atomic-publish step), so
  `trace-expires` is computed directly as `now.date_naive() + retention_days`
  — no string round-trip needed on this path. Injected via
  `NoteContent::frontmatter_additions`, NOT by passing `StagingConfig` into the
  renderer (the design's committed answer to the module-boundary question).

### Deviations
- The design phrased Phase 3 as "compute = date(parse(ingested)) +
  retention_days, parsing the dual ingested format." On the publish path the
  ingested instant is `now` (a `DateTime`), so we skip the parse and compute
  from `now.date_naive()` directly. Same result, no redundant
  serialize/parse. The dual-format `parse_ingested_date` IS exercised — it is
  the Phase 4 backfill path and is unit-tested here in Phase 3 since the helper
  lives in retention.rs.

### Tradeoffs
- Helpers live in `retention.rs` rather than a new module: retention already
  owns `retention_days` semantics and imports chrono, so it is the natural
  home and avoids a one-function module.

### Open questions
- None. Reingest re-stamping is automatic: every publish recomputes from a
  fresh `now`, so a reingest gets a fresh `trace-expires`.

## Phase 4: backfill legacy notes

### Design decisions
- Added `apply_trace_expires` to `borg/src/pipeline/atomic.rs`, backed by a new
  private `insert_or_replace_field(rendered, key, value, anchors)` helper. The
  trace-expires line is inserted after `ingested:` (falling back to `date:`,
  then the opening `---`). `apply_ingested_date` was left untouched (working,
  tested code) rather than refactored onto the shared helper — surgical.
- `classify_for_backfill` was decoupled into two independent decisions: a
  target `ingested` value (the existing homogenization, minus its early
  returns) and a target `trace-expires` (only when the note has a non-empty
  `trace:`). The note is skipped only when NEITHER field needs writing — the
  skip predicate now keys on `trace-expires` presence/correctness, not on the
  `ingested` check (the design's required change).
- trace-expires is computed from the EFFECTIVE ingested date (the value
  ingested will hold after this run), via `retention::parse_ingested_date`, so
  a note getting its ingested homogenized in the same pass stamps an expiry
  consistent with the new ingested.
- `precise` still counts only receipt-derived ingested writes (`is_precise &&
  write_ingested`), so adding a trace-expires-only write never inflates it.

### Deviations
- None from the resolved decisions. Receipts-unavailable computes trace-expires
  from the midnight-fallback ingested (best-effort, precise=false) rather than
  skipping — exactly resolved-decision #6.

### Tradeoffs
- Kept `apply_ingested_date` as-is and added a parallel generic helper rather
  than unifying both onto `insert_or_replace_field`. Slight duplication of the
  insert-position logic, but zero risk to the already-shipped ingested path.
- Reused the existing `BackfillReport` counters (no new trace-expires-specific
  counter). `backfilled` counts notes touched on either field; the design's
  test list didn't ask for a separate count and the dry-run log line names both
  fields, so observability is preserved without a schema change to the report.

### Open questions
- None.

## Phase 5: advertise the trace block to the consuming LLM

### Design decisions
- The capability sentence was added to `get_info` server instructions and to the
  12 note-returning `#[tool(description = ...)]` strings in
  `oracle/src/server.rs`. The load-bearing marker substring is `` `trace` block ``.
- Wording calls out transcripts explicitly ("e.g. a full transcript") per the
  decided framing, while staying generic enough to cover any staged source
  (matches the design's "transcripts being the richest case" goal).
- The regression guard (`note_returning_tools_advertise_trace_block` in
  `oracle/src/server/tests.rs`) lists the 12 note tools and 7 non-note tools
  explicitly, then a coverage assertion fails if `list_tools()` advertises any
  tool absent from both buckets, so a future tool must be classified.

### Deviations
- None from the decided approach (handle-only, broad, transcripts called out).

### Tradeoffs
- Inlined the same sentence in each of the 12 descriptions rather than factoring
  a shared `const`: the rmcp `#[tool(description = ...)]` macro wants a string
  literal, and a `concat!` would obscure the human-readable text the macro
  emits. The regression test pins all 12 in sync, so duplication can't drift.

### Open questions
- None. The generic-vs-transcript framing question is resolved: transcripts are
  called out as the motivating case.
