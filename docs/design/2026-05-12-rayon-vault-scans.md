# Design Document: Rayon parallelization for vault-scan and CPU-bound per-note operations

**Author:** Scott Idler
**Date:** 2026-05-12
**Status:** Implemented (2026-05-12)
**Review Passes Completed:** 5/5 + Architect
**Shipped in:** v0.5.40

## Summary

`vault::note::scan_vault` and several CPU-bound per-note loops across `borg` and `cortex` walk the ~1135-note vault sequentially today. This doc ports them to `rayon::par_iter` where the work is genuinely CPU-bound or file-I/O-bound-per-note, leaving async/LLM-bound loops untouched. Scope is a focused refactor of identified hot scans; the API contracts (`Vec<Note>` in, deterministic output out) do not change.

## Problem Statement

### Background

The `vault::note::scan_vault` helper is the workhorse of cross-tool note iteration. It walks the entire Obsidian vault with `WalkDir`, reads each `.md` file from disk, parses YAML frontmatter, and returns `Vec<Note>` sorted by path. Today the implementation is fully sequential (`vault/src/note.rs:37`):

```rust
for entry in WalkDir::new(vault_root)... {
    match parse_note(vault_root, path) { ... }
}
notes.sort_by(|a, b| a.path.cmp(&b.path));
```

Multiple operations across the workspace re-walk the vault (or operate on the loaded `&[Note]`) sequentially:

- `borg::backfill::run_backfill_ingested` (1135-note scan, frontmatter parse, conditional `write_atomic`)
- `borg::audit::build_note_index` (1135-note scan, frontmatter parse, index into `HashMap<String, Vec<PathBuf>>`)
- `cortex::autotag::lint_autotag` (`for note in notes` over the parsed vault)
- `cortex::quality::lint_quality` and `cortex::quality::apply_quality` (per-note assessment, may write)
- `cortex::migrate` (multiple per-note loops)

These run on workstations with 16+ hardware threads. Sequential traversal is the bottleneck, not network or disk throughput.

### Problem

For large-vault operations (anything that touches more than a few notes), wall-clock time is bounded by single-thread parse+walk speed. The 2026-05-12 `backfill-ingested` run scanned 1135 notes and wrote 498 of them; on saturated I/O this is a non-trivial wait, and operations like `cortex sweep` over the full vault compound the problem because they scan, then lint, then apply, then re-walk for diffs.

The ingestion pipeline (the HTTP `/ingest` path) is *not* affected: each ingest is its own `tokio::spawn` task and already runs N-in-parallel as of `v0.5.39`. The issue is the offline / maintenance commands.

### Goals

- **Parallelize vault-wide file reads and frontmatter parses.** `scan_vault` returns the same `Vec<Note>` (same order, same content) in a fraction of the wall-clock.
- **Parallelize the CPU-bound per-note compute stages** in `cortex::autotag`, `cortex::quality`, `borg::backfill`, `borg::audit`. Same outputs; deterministic ordering preserved where callers depend on it.
- **Preserve API contracts.** No caller has to re-shape its code; the parallel implementations live behind the same function signatures.
- **No regression in error semantics.** A file that fails to parse still produces the same `log::warn!` and is excluded from the result.

### Non-Goals

- **Parallelize the ingestion pipeline.** It is already async-parallel via `tokio::spawn`. Rayon would be wrong (blocks OS threads under a tokio runtime; default rayon pool starves at `num_cpus`).
- **Parallelize cortex::classify's LLM-call loops.** These are `async` and call remote LLM APIs; they must stay tokio-based. They are explicitly out of scope.
- **Reduce memory footprint of `scan_vault`.** Today every note's `raw`, `body`, and `frontmatter` is held in `Vec<Note>`; that stays.
- **Change the on-disk write protocol.** Per-note writes still go through `write_atomic`, which is per-file safe under concurrent writes to *different* files.
- **Touch `oracle`.** Oracle is a long-running MCP server that uses sqlite for queries; no vault walks to parallelize.
- **Async runtimes for the offline commands.** Backfill, audit, cortex-CLI subcommands are blocking by design; the parallelism is rayon's blocking-thread-pool, not tokio's.

## Proposed Solution

### Overview

Three layers of change, each independently shippable:

1. **`vault::note::scan_vault`** (one function, every caller benefits): replace the `for entry in WalkDir { ... }` body with a two-phase pattern - collect paths sequentially via WalkDir (fast, no I/O), then `paths.par_iter().filter_map(parse_note).collect()`.

2. **Per-note compute loops in `cortex::autotag::lint_autotag`, `cortex::quality::lint_quality`, `cortex::quality::apply_quality`** (sync `fn` consumers of `&[Note]`): replace `for note in notes` with `notes.par_iter()` patterns. These are pure CPU work over independent notes; trivial conversion.

3. **`borg::backfill::run_backfill_ingested` and `borg::audit::build_note_index`** (file-walk + read + parse + maybe write): convert the per-path inner loop to `par_iter`. Writes go to distinct files; intake/ledger/dlq lock contention is a non-issue because these scans don't touch those tables.

### Architecture

```
                    ┌──────────────────────────────────────────────┐
                    │  vault::note::scan_vault                     │
                    │   1. walk dir (sequential, cheap)            │
                    │   2. par_iter(read+parse) -> Vec<Note>       │
                    │   3. sort by path (deterministic order)      │
                    └──────────────────────────────────────────────┘
                                       │
              ┌────────────────────────┼─────────────────────────┐
              ▼                        ▼                         ▼
      cortex::autotag           cortex::quality          borg / cortex
      lint_autotag              lint_quality             migrate ops
      (par_iter compute)        apply_quality            (per-note loops
                                (par_iter compute        already over Note
                                 + per-file write)        slices)
```

Side scans that re-walk independently of `scan_vault`:

```
   borg::backfill::run_backfill_ingested       borg::audit::build_note_index
   ───────────────────────────────────         ──────────────────────────────
   collect_md_files (seq)                      collect_md_files (seq)
        │                                            │
        ▼                                            ▼
   par_iter:                                    par_iter:
     read + extract_fm + maybe write              read + extract_fm
        │                                            │
        ▼                                            ▼
   atomic counters for report                  Mutex<HashMap> aggregator
```

### Data Model

No struct changes. Output types remain:

| Function | Input | Output (unchanged) |
|---|---|---|
| `vault::note::scan_vault` | `&Path, &ScanConfig` | `Result<Vec<Note>>` (sorted by `path`) |
| `cortex::autotag::lint_autotag` | `&[Note], &[Note], &AutoTagConfig` | `Report` |
| `cortex::quality::lint_quality` | `&[Note], &QualityConfig` | `Report` |
| `cortex::quality::apply_quality` | `&Path, &[Note], &QualityConfig` | `Result<usize>` |
| `borg::backfill::run_backfill_ingested` | `&Config, dry_run: bool` | `Result<()>` (prints `BackfillReport`) |
| `borg::audit::build_note_index` | `&Path, &[String]` | `Result<HashMap<String, Vec<PathBuf>>>` |

For `BackfillReport`-style counter accumulators, switch internal counters to `AtomicUsize` so workers update them lock-free. Final `Report` shape unchanged.

### API Design

No public-API changes. Function signatures, error types, and log output all stay identical. Callers do not need to recompile their understanding of the API - they just see faster execution.

Internal helper additions (private to each module):

```rust
// vault/src/note.rs - new helper, used inside scan_vault
fn collect_md_paths(vault_root: &Path, scan_config: &ScanConfig) -> Result<Vec<PathBuf>> { ... }
```

### Implementation Plan

#### Phase 0: fix the async/sync boundary (Option A: collapse vault-walk fns to sync)
**Model:** sonnet

**Pre-requisite to Phase 1.** The Architect review identified that the original Phase 0 plan (`tokio::task::spawn_blocking(|| run_backfill_ingested(...))`) was unsound: `spawn_blocking` expects `FnOnce() -> R`. Passing an `async fn` constructs an unpolled `impl Future` and immediately drops it, so the function body silently does not execute. The fix is to remove `async` from the vault-walk functions entirely, not to wrap them.

Grep audit results (verified 2026-05-12):

- `borg::backfill::run_backfill_ingested` is `pub async fn` at `borg/src/backfill.rs:87` and has **zero `.await` calls** in its body. The `async` keyword is decorative; the function does only CPU + blocking I/O.
- `borg::audit::run_audit` is `pub async fn` at `borg/src/audit.rs:68` and likewise has **zero `.await` calls** in its body.
- `cortex/src/daemon.rs::start_watching` is `async fn` and calls `crate::vault::scan_vault(...)` directly at four sites.

Option A plan (decided after Architect review):

- **Step 0.1** Drop `async` from `borg::backfill::run_backfill_ingested` and `borg::audit::run_audit`. Drop the corresponding `.await` from the two call sites in `borg/src/main.rs:62` and `borg/src/main.rs:112`. This is a mechanical rename; both functions are async-in-name-only today.
- **Step 0.2** Audit any other vault-walk `async fn` for the same pattern. If the body has zero `.await` calls, convert to `sync fn` in the same commit. If the body does have legitimate awaits (e.g., LLM calls), leave it async and document why; those are out of scope for this design doc.
- **Step 0.3** In `cortex/src/daemon.rs`, the sync `run_configured_actions(...)` and `run_classify_only(...)` calls (invoked from inside the async `start_watching` `tokio::select!` loop) are the actual call sites where blocking CPU work re-enters the tokio runtime. Wrap each of those two call sites in `tokio::task::block_in_place(|| ...)`. **Implementation note:** `block_in_place` is preferred over `spawn_blocking` here because `cortex::config::Config` is not `Clone`; `spawn_blocking` requires a `'static + Send` closure, which would force either adding `Clone` derives across the entire config tree or wrapping the daemon's `Config` in `Arc` at every borrow point - scope creep neither the design nor the Architect review required. `block_in_place` has no `'static`/`Send` bound on its closure and tells the tokio scheduler to move other tasks off the current worker for the duration of the blocking call, which addresses the same runtime-starvation concern. Requires a multi-thread tokio runtime; verified `#[tokio::main]` is the default (multi-thread).
- **Step 0.4** Add a clippy lint exception for `clippy::unused_async` if needed during the transition, but the goal is a clean sync signature with no residual `async` decoration.
- **Tests:** existing daemon tests must pass; add a smoke test that calling `scan_vault` from within a tokio runtime via `spawn_blocking` does not panic. Add a smoke test that `borg backfill-ingested --dry-run` runs to completion from the CLI.
- **Ship-on-its-own:** Phase 0 is valuable independent of rayon. Land it first as a correctness fix.

#### Phase 1: vault::note::scan_vault
**Model:** sonnet

- Add `rayon = { workspace = true }` to `vault/Cargo.toml` (and to the workspace `Cargo.toml` if not present).
- Split `scan_vault` into `collect_md_paths` (sequential WalkDir, no I/O beyond directory reads) + `paths.par_iter().filter_map(|p| parse_note(root, p).ok()).collect()`.
- Preserve the existing `log::warn!` for parse failures via `inspect_err`.
- Preserve the final `sort_by(path)` for deterministic ordering.
- Unit tests: extend existing `scan_vault` tests to assert ordering and parse-failure logging are preserved under parallel collection.
- Benchmark: add `tests/perf.rs` (ignored by default) measuring 1000-note tempdir vault scan; record before/after.

#### Phase 2: cortex per-note CPU loops
**Model:** sonnet

- Add `rayon` to `cortex/Cargo.toml` (workspace dep).
- `cortex/src/autotag.rs::lint_autotag`: replace `for note in notes` with `notes.par_iter().flat_map(assess_note_for_autotag).collect()` (pure compute, no writes).
- `cortex/src/quality.rs::lint_quality`: same pattern; `assess_note` is already a pure function over `&Note`.
- `cortex/src/quality.rs::apply_quality` (split into two sub-phases):
  - **2a: read+compute path.** Parallelize only the per-note *assessment* (pure CPU). `par_iter` over notes producing a `Vec<Violation>` sequentially merged.
  - **2b: write path - parallelize unconditionally; fsync-gate does not apply to cortex.** The Architect's fsync-contention concern was specific to `borg::pipeline::atomic::write_atomic`, which opens the parent directory and calls `sync_all()` at `borg/src/pipeline/atomic.rs:38-39`. Inspection confirmed cortex's `apply_quality` and `apply_autotag` use plain `std::fs::write`, not `write_atomic`. Plain `std::fs::write` does no explicit parent-directory fsync, so the kernel-level dirent-sync serialization that gates Phase 3 (where borg uses `write_atomic`) does not gate Phase 2b. Parallelize the write loops unconditionally with rayon. Error propagation via `par_iter().try_reduce(...)` to keep the existing fail-fast `Result<usize>` semantics. Counter is `usize` aggregated via reduce; no `AtomicUsize` needed.
- Verify no caller is on an async runtime (these are sync `fn`; daemon already calls them from `tokio::task::spawn_blocking` after Phase 0).
- Tests: cargo test - existing tests should pass unchanged; add one that asserts `lint_quality` produces the same `Report` whether called sequentially or in parallel on a fixed corpus.

#### Phase 3: borg::backfill + borg::audit
**Model:** sonnet

- **Prerequisite:** Phase 0 already converted `run_backfill_ingested` and `run_audit` to sync `fn`. This phase parallelizes their inner loops.
- `borg::backfill::run_backfill_ingested`: convert the `for path in &md_files` loop to `md_files.par_iter().for_each(|p| ...)` with `AtomicUsize` counters for `BackfillReport` fields. Print the report after collection. Note: writes here have the same parent-directory `fsync` contention concern as Phase 2b; the backfill 2026-05-12 data showed 498 writes across many daily folders, so contention pressure should be lower than `apply_quality`'s clustered writes - but the same benchmark gate applies. If sequential writes beat parallel writes on a realistic fixture, parallelize the read+filter path only and keep writes sequential.
- `borg::audit::build_note_index`: convert to `par_iter().filter_map(|path| extract_source_url(path).map(|url| (url, path.clone()))).collect::<Vec<(String, PathBuf)>>()`. Rayon's `.collect()` preserves input order, so the per-URL `Vec<PathBuf>` ordering matches the pre-sorted `collect_md_files` output bit-for-bit. After the parallel collection, fold the `Vec<(String, PathBuf)>` into the `HashMap<String, Vec<PathBuf>>` on the main thread with a sequential loop. This is faster than `fold + reduce` (no HashMap merging) and trivially deterministic. The Architect raised the determinism concern; this is the agreed fix.
- Unit tests: add a fixture vault of 50 notes with known source URLs and verify the index is identical to the sequential version (same key set, same values per key, in identical order per key).

#### Phase 4: scan-heavy migrate ops
**Model:** sonnet

- `borg::migrate` and `cortex::migrate` have similar per-note loops. Apply the same `par_iter` conversion where the work is pure or per-file safe.
- These commands run rarely (one-shot migrations); shipping them in their own phase keeps the change isolated from the hot-path Phase 1/2 work.

#### Phase 5: docs + post-merge audit
**Model:** sonnet

- Update `CLAUDE.md` to mention "vault-scan operations parallelize via rayon; respect the rule that async/LLM loops stay tokio-based."
- Run `cargo bench` / the perf harness against a fresh vault clone; record numbers in this design doc's "Performance" section (post-implementation, factual).
- Confirm `otto ci` green across the workspace.

## Alternatives Considered

### Alternative 1: Don't bother - the vault is small
- **Description:** ~1135 notes is small. Sequential scans probably complete in under a second. Skip the refactor.
- **Pros:** Zero work. Zero risk.
- **Cons:** The vault keeps growing (the ingestion pipeline adds notes daily). At ~10k notes the same scans become 10x slower and start being human-noticeable. The fix is mechanical; deferring it bakes the bottleneck in.
- **Why not chosen:** The cost of the refactor is low and the runway is short. Phase 1 alone is a single function rewrite.

### Alternative 2: Use tokio's `spawn_blocking` per file
- **Description:** Spawn one tokio blocking task per file read.
- **Pros:** Stays within the tokio model; no new dependency type.
- **Cons:** tokio's blocking-pool defaults to 512 threads and is meant for occasional blocking calls inside async code, not bulk file scans. Heavy use confuses the runtime and starves real async work. Rayon is the idiomatic answer for data-parallel CPU + file I/O.
- **Why not chosen:** Wrong tool for the job. Rayon already exists in the workspace (`borg/Cargo.toml`) for similar reasons.

### Alternative 3: Async file I/O via `tokio::fs::read_to_string`
- **Description:** Convert `parse_note` to `async fn` and await `tokio::fs::read_to_string` for each file.
- **Pros:** Fits an all-async codebase.
- **Cons:** `tokio::fs` is a thin wrapper around `spawn_blocking(std::fs::...)`; it doesn't make file I/O faster, just async-friendly. Async-converting `scan_vault` would force every caller to be async and to live inside a runtime, which is the wrong direction for offline CLI commands.
- **Why not chosen:** No throughput benefit; large blast radius of API change.

### Alternative 4: sqlite-backed index (defer to `2026-04-20-sqlite-ledger-and-views.md`)
- **Description:** Replace `scan_vault` entirely with sqlite queries over an indexed mirror of the vault. The sqlite design doc already covers this.
- **Pros:** O(log n) lookups, no walk at all.
- **Cons:** Out of scope; that design doc is its own multi-phase effort. Even with sqlite, building/refreshing the index requires walking and parsing the vault - and that build phase still benefits from rayon.
- **Why not chosen:** Forward-compatible. This design doc speeds up the walk-and-parse work whether or not sqlite arrives later.

## Technical Considerations

### Dependencies

- **`rayon` (workspace dep).** Already in `borg/Cargo.toml` at `1.12.0`. Add to `vault/Cargo.toml` and `cortex/Cargo.toml`. No new external dependencies; no new transitive risk.
- **No tokio runtime requirements added.** Rayon's blocking-thread pool is independent of any tokio runtime.

### Performance

**Pre-implementation expectations (to be verified, not asserted as fact):**

- Walking 1135 files with `WalkDir` is fast (single-digit milliseconds on a warm FS cache). Not the bottleneck.
- The per-file work is: `fs::read_to_string` (kernel cache hit, ~10-100µs per small file), `parse_frontmatter` (small YAML, ~50-500µs CPU), allocation. Sum-of-work ~100-500ms sequentially.
- On a 16-thread workstation, rayon's `par_iter` should drop wall-clock toward the slowest single-file time + overhead. Plausible target: 30-80ms.
- For `cortex::quality::apply_quality` which also writes, the savings depend on the proportion of notes that actually require a write. Backfill data point: 498/1135 = 44% write rate in the recent ingested-backfill, suggesting a meaningful speedup is realistic but not 16x.

**Benchmark plan:** add a perf test that builds a fresh 1000-note tempdir vault and times scan + per-note assessment. Record both numbers in the doc body after Phase 1 lands.

**Measured (Phase 1, release-mode on this workstation):** `cargo test --package vault --test perf --release -- --ignored --nocapture` produced `scan_vault(1000 notes) -> 1000 parsed in 8.5ms`. This is a single-machine baseline, not normative; the harness is checked in (`vault/tests/perf.rs`, `#[ignore]`) so other machines can reproduce. The sequential pre-implementation number was not captured in the same harness, so the speedup ratio is not in this doc; the qualitative observation is that 1000 notes parse comfortably under 10 ms wall-clock on a 16-thread workstation, which is below the threshold at which any user-perceptible wait remains. The harness exists for future regression detection if scan_vault ever creeps back upward.

### Security

- No new attack surface. The rayon thread pool runs the same code on the same inputs; the only change is execution shape.
- File-lock contention: nil. Each parallel worker opens a distinct file; the ledger/intake/dlq markdown locks are not held during these scans.
- Concurrent `write_atomic` to distinct files: safe by construction (tempfile + rename is per-file atomic).

### Testing Strategy

- **Unit-test invariant:** for every parallelized function, add a test that constructs a deterministic small corpus and asserts the parallel output equals the sequential output bit-for-bit (or set-equal for unordered outputs like HashMaps).
- **Determinism test:** `scan_vault` must return `Vec<Note>` sorted by `path` whether parallel or sequential. Test asserts the sort key holds under both.
- **Error-path test:** create a vault with one note containing malformed frontmatter; assert the parallel scan emits the same `log::warn!` and excludes that note, exactly like the sequential version.
- **Counter-correctness test for backfill:** parallel run with known input produces the same `BackfillReport` field values as the sequential baseline.
- **Existing tests:** all current tests should pass unchanged. If any do not, the change is wrong.

### Rollout Plan

1. **Phase 0** (`spawn_blocking` wrapper around the daemon's `scan_vault` calls) ships first. Standalone correctness fix; valuable independent of rayon.
2. **Phase 1** (`vault::note::scan_vault` parallelized) ships next. All callers benefit transparently; revertable in isolation if regression appears.
3. **Phases 2 and 3** ship together once Phase 1 is stable - they consume the parallel `scan_vault` but are independent rewrites.
4. **Phase 4** (migrate ops, rarely run) ships last.
5. **Phase 5** (docs + perf measurement) is the close-out: record actual numbers in this doc body and update CLAUDE.md.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Async/sync mismatch: `spawn_blocking(|| async_fn(...))` silently drops the Future | **Resolved by Phase 0 (Option A)** | High | The original Phase 0 design proposed wrapping `run_backfill_ingested` in `spawn_blocking`; verified this would not work because the function is `async fn`. Phase 0 now converts the function to sync `fn` instead (it has zero internal `.await`s). Daemon callers use `spawn_blocking` only for sync-fn targets. |
| Rayon thread starvation inside the cortex daemon's tokio runtime | **Confirmed present today** | High | Verified: `cortex/src/daemon.rs::start_watching` is `async fn` and calls `scan_vault` directly. Phase 0 wraps those call sites in `tokio::task::spawn_blocking` BEFORE Phase 1 lands. Requires a multi-thread tokio runtime (Phase 0 documents and asserts this). |
| Parent-directory `fsync` lock contention from parallel `write_atomic` calls | Med | High | `write_atomic` opens the parent dir and calls `sync_all` (`borg/src/pipeline/atomic.rs:38-39`). On ext4/XFS, directory-metadata syncs serialize at the inode level, so many concurrent writes to the same daily folder may underperform sequential writes. Mitigation: Phase 2b and Phase 3 writes are gated on a benchmark that compares sequential vs. parallel write throughput on a realistic clustered fixture; if parallel does not win, the read+compute path is parallelized but writes stay sequential. |
| Parallel-write data corruption from `apply_quality` racing on the same path | Negligible | High | Per-note writes target distinct files; `write_atomic` (tempfile + rename) is per-file safe. Tests verify no path appears twice in the violation set. This is the correctness side of the fsync row above; correctness is fine, throughput is the open question. |
| Non-deterministic log order from parallel `warn!` emissions | High | Low | Acceptable. Order of warnings was never load-bearing; the count and content are preserved. |
| Memory pressure from holding more file contents in flight simultaneously | Low | Low | Peak resident memory for `scan_vault` is dominated by the final `Vec<Note>` (every note's `raw`, `body`, `frontmatter` is allocated in the result; ~5-10 MB for the current 1135-note vault). The parallel implementation adds a transient working-set *delta* of roughly `num_cpus * avg_note_size` (~80 KB on a 16-thread workstation with ~5 KB notes) while reads are in flight, on top of the same final allocation. The delta is negligible; the peak is what it always was. |
| Determinism regression: parallel output not bit-equal to sequential output | Med | Med | Post-collection `sort_by(path)` (already present) guarantees stable ordering for `scan_vault`. For `build_note_index`, the design uses `par_iter().filter_map().collect::<Vec<_>>()` followed by sequential HashMap insertion to preserve per-key value ordering. Tests assert bit-equality; CI catches drift. |
| Some `for note in notes` loops have hidden inter-iteration state (counter mutation, ad hoc dedup) | Med | Med | Inspected during implementation: convert to `AtomicUsize` for counters and `par_iter().filter_map().collect()` for accumulators; avoid `fold + reduce` patterns that obscure ordering. Tests assert equivalence. |

## Open Questions

- [x] **Resolved.** Does `cortex daemon` invoke `scan_vault` from inside a tokio task without `spawn_blocking`? **Yes** (verified via grep of `cortex/src/daemon.rs::start_watching`). Folded into Phase 0.
- [x] **Resolved (Architect review, 2026-05-12).** Original Phase 0 plan to `spawn_blocking(|| run_backfill_ingested(...))` was unsound because that function is `async fn`. Grep confirmed both `run_backfill_ingested` and `run_audit` are async-in-name-only (zero internal `.await`s). Option A (drop the `async` keyword) is the resolution; Phase 0 rewritten accordingly.
- [x] **Resolved (Architect review, 2026-05-12).** `build_note_index` determinism under `par_iter`. Use `par_iter().filter_map().collect::<Vec<_>>()` + sequential HashMap insertion, not `fold + reduce`. Phase 3 rewritten.
- [ ] Phase 2b and Phase 3 parallel-write throughput gate: parent-directory `fsync` may serialize at the kernel level. Benchmark required before merging the parallel write path. If parallel does not win, ship the read+compute parallelization only.
- [ ] Is `cortex::state::FileEntry` indexing (`cortex/src/state.rs:43`) a hot path or a once-at-startup cost? Likely once-at-startup (it builds the file manifest for change-detection). Worth a quick perf check during Phase 1's benchmark work; if not on the critical path, skip parallelization.
- [ ] Do any callers of `scan_vault` rely on natural FS traversal order (not the post-sort order)? Quick audit before Phase 1: `rg 'scan_vault\(' cortex/src borg/src`. Tentative answer: no - the function's contract has always been path-sorted output; callers that wanted other orders re-sort.
- [ ] Should the per-note `parse_note` failure rate be exposed as a metric? Today it's `log::warn!`-only. Out of scope for this doc, but if parallelism increases the rate of warnings emitted in bursts, the operator-side experience could surface this.
- [ ] `cortex::classify` has per-note loops that issue LLM calls and others that are pure heuristic classification. Should the pure parts be parallelized in this doc, or carved into a follow-up? **Tentative: follow-up.** Each call site needs per-loop audit to confirm purity; that's a larger surface than the obviously-pure `lint_autotag` / `lint_quality` covered here.
- [x] **Resolved.** Workspace `Cargo.toml` does not currently host a `[workspace.dependencies]` table for `rayon`. **Decision:** add a workspace dependency entry so all four crates use `rayon = { workspace = true }`; version drift across crates is then impossible.
- [ ] **Deferred (post-implementation):** `borg::migrate::run_migrate` (`borg/src/migrate.rs:10`) was left sequential during the v0.5.40 rollout. Unlike `cortex::apply_field_transforms` and `cortex::apply_value_transforms` (parallelized in v0.5.41), this loop has inter-iteration state: it accumulates `ledger_entries: Vec<LedgerEntry>` during the per-note pass and seeds the Borg Ledger after the loop completes. Parallelizing it would need the same par-classify + sequential-write split that `run_backfill_ingested` uses, plus a similar treatment of the `ledger_entries` accumulator. The migrate command runs as one-shot maintenance, so the wall-clock win does not currently justify the structural surgery. Revisit if the migration corpus grows past the point where the sequential pass becomes a noticeable wait.

## References

- `docs/design/2026-04-20-sqlite-ledger-and-views.md` - future sqlite migration; forward-compatible with this work
- `docs/design/2026-05-11-borg-intake-log-and-dlq.md` - the intake/DLQ work that introduced `borg::backfill`
- `vault/src/note.rs:37` - the `scan_vault` implementation to parallelize
- `borg/src/slides.rs:293` - existing rayon usage in the workspace (frame-hash parallelism)
- `cortex/src/autotag.rs:9`, `cortex/src/quality.rs:71` - target per-note loops
- 2026-05-12 observed slam: 21 concurrent ingest pipelines completed via `tokio::spawn` in `/ingest`. Establishes that the workspace's existing concurrency model is async-per-pipeline; rayon-per-walk complements rather than conflicts with it.
