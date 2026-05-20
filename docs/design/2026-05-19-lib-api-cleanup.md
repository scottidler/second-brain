# Design Document: finish the sb lib API cleanup

**Author:** Scott Idler
**Date:** 2026-05-19
**Status:** In Review
**Review Passes Completed:** 5/5

## Summary

The `unified-sb-binary` refactor shipped a working single bin but left two specific gaps that the architect audit caught: the lib crates still emit ~200 `println!` calls (the design said "lib returns typed data; never println!s") and Phase 2 status/doctor skipped two checks the design explicitly mandated. Separately, the `run_` prefix on every lib pub fn (`run_ingest`, `run_lint`, `run_dlq_list`, …) is noise once clap parsing lives entirely in `sb`. This doc finishes the lib API: drop the `run_` prefix, restore the lib-returns-data invariant, apply rust.md naming rules tree-wide, and add the missed Phase 2 checks.

## Problem Statement

### Background

The 2026-05-19 unified-sb-binary refactor consolidated `borg`/`cortex`/`oracle` into one `sb` bin with the three subsystems as lib-only crates. The design specified a shell/core split: clap parsing in `sb`, business logic in lib, typed data flowing across the boundary. The implementation honored the structure (one bin, three libs) but the call signatures kept the old shape: every entry point is named `run_<verb>` and most still emit output directly via `println!`/`eprintln!` rather than returning typed data for `sb` to format.

Two architect implementation audits caught the gap. The first round identified obvious misses (the bootstrap stub, the stale `systemd/` references, the orphaned `oracle/src/cli.rs`); the second round identified the structural gap: 154 `println!` calls remain in `borg/src/`, 69 in `cortex/src/`, and the architect proved that the "iterative print is architecturally hard" rationale I claimed in the Phase C cleanup was wrong for most of cortex (sweep/state/intel/embed all print at function tails over pre-computed data; only borg's actual pipeline-running verbs like `reingest`/`replay`/`migrate --apply` have a real buffering tradeoff).

#### Triage table that produced this doc

After the architect's second audit, the remaining work was classified as follows (this table is the inflection point — Scott directed "no documented debt", which means every row including the bottom one ships rather than gets deferred):

| Item | Effort | Architecturally design-y? |
|---|---|---|
| cortex `{state, sweep, intel, embed}` println cleanup | ~1 hr | No — architect proved this is mechanical (same Report-typed-return pattern already used in `run_lint`/`migrate`/`link`/`classify`); the "iterative" rationale I'd given was false |
| Shared-config drift check in `checks.rs` | ~15 min | No — file hash compare, parallel to existing `pattern_findings` |
| Config parse-status (`serde_yaml::from_str` per config) | ~15 min | No — bounded extension to `config_findings` |
| Document `rmcp` dep in `unified-sb-binary.md` | 5 min | No |
| borg's truly-iterative handlers (`reingest`, `replay`, `migrate --apply`, `audit --fix`) | Real refactor | Yes — buffering vs callback IS a real architectural choice. This doc decides: **buffer**. See Alternative 3. |
| Naming refactor: drop `run_` prefix; apply rust.md naming rules (plural-s; single-word filenames; module path as namespace) | Tree-wide rename | No — mechanical, but high call-site churn. Pairs with the print refactor since both touch the lib API contract; doing them together amortizes the churn. |

**User directive that anchored this doc:** "we are not leaving any 'documented debt'." That makes Alternative 1 (leave-it-and-document) explicitly off the table. The remaining decisions are about how to do the work, not whether.

### Problem

Three coupled defects on the lib API surface:

1. **Lib still uses `println!`/`eprintln!`.** The prior design doc said "Each existing crate's `lib.rs` exposes one public function per subcommand … returns typed output; never `println!`s. Output formatting happens in `sb/src/cli/<subsystem>.rs` after the lib call returns." This is true for the four `cortex` verbs cleaned up in Phase C (`lint`/`migrate`/`link`/`classify`), for `oracle::run_index/run_call/run_list/run_stats`, and for `borg::blocklist::run_*`. It is NOT true for everything else. The boundary is porous: 154 sites in `borg/src/`, 69 in `cortex/src/`. The architect's specific findings:
   - `cortex::sweep::run_sweep` — 7 printlns, all at branch terminals after pre-computed work
   - `cortex::run_state` — diff prints at end over pre-computed `diff.added/removed/modified` vectors
   - `cortex::intel::run_intel` — exactly 2 printlns at function end
   - `cortex::embed::run_embed` — 2 printlns (one at prefetch, one at final stats)
   - `borg::run_reingest`/`replay::run`/`audit::run_audit`/`migrate::run_migrate` — DO print iteratively during real I/O loops; real architectural tradeoff
   - `borg::run_note`/`run_ingest`/`run_file_ingest`/`run_sign`/`run_hotkey` and the systemd installers (`install_systemd`, `uninstall_systemd`) — print at terminals; mechanical to refactor

2. **`run_` prefix is noise.** Every lib pub fn is `run_<verb>`: `borg::run_ingest`, `borg::run_audit`, `cortex::run_lint`, `cortex::embed::run_embed`, `oracle::run_serve`, `borg::triage::run_dlq_list`, `borg::triage::run_intake_show`. This came from clap-handler-style naming when each `run_*` matched one CLI verb dispatched by `match cli.command`. With clap gone from the lib (it lives in `sb/src/cli/`), the prefix is dead weight: `borg::ingest(opts)` reads better than `borg::run_ingest(opts)`. The Rust ecosystem convention is bare verbs (`tokio::spawn`, `serde::deserialize`, `cargo::ops::clean`), not action-prefixed names. Double-namespacing makes this worse: `borg::triage::run_dlq_list` is "triage's run_dlq_list", where both `triage` and `dlq` are already in the path.

3. **Phase 2 missed two checks the design called out.** Per the original Phase 2 spec, `sb status`/`sb doctor` were supposed to surface:
   - Pattern-sync drift (implemented ✓)
   - Embedding model presence (implemented ✓)
   - Systemd state (implemented ✓)
   - Recent intake/ledger/dlq counts (implemented Phase B ✓)
   - Oracle vault stats (implemented Phase B ✓)
   - Real embedding coverage (implemented Phase B ✓)
   - **Shared-config drift** — comparing `config/{canonical-tags,tag-mapping,tag-proposals}.yml` in the repo against `~/.config/second-brain/*.yml`. Skipped.
   - **Config presence AND parse status** — current `config_findings()` only does `path.exists()`; the design said "config presence and parse status", meaning try to `serde_yaml::from_str` each config file and surface parse errors. Skipped.

4. **`sb/Cargo.toml` now depends on `rmcp`** (added during Phase C so `sb/src/cli/oracle.rs` can format `rmcp::model::CallToolResult` and `rmcp::model::Tool`); the prior design doc's dependency list doesn't mention it.

5. **Two test filenames violate the `no underscores in .rs filenames` rule** in `~/repos/scottidler/claude/HOME/repos/.claude/rules/rust.md`:
   - `vault/tests/regression/hybrid_retrieval.rs`
   - `vault/tests/regression/candle_parity.rs`

### Goals

- **Lib invariant restored:** zero `println!`/`eprintln!` in `borg/src/`, `cortex/src/`, `oracle/src/` — both public AND private functions. Private helpers (`install_systemd`, `install_launchd`, `show_status`, etc.) are called from pub fns and propagate prints through the lib boundary; they get the same treatment. Every pub fn returns `Result<TypedData>` or `Result<()>` (when the function has no payload, e.g., daemon long-running loops); `sb` owns all stdout/stderr/exit-code mapping. Log macros (`log::info!`/`debug!`/`warn!`/`error!` and `tracing::*`) are exempt — they route through the logger initializer and do not touch stdout.
- **Drop the `run_` prefix** from all lib pub fns. Use bare verbs/nouns in their owning module: `borg::ingest`, `borg::audit::check`, `borg::triage::dlq_rows`, `cortex::lint`, `cortex::embed::run`, `oracle::serve`. Where a module is named after the verb it serves (e.g., `borg::audit::audit` would collide), use a complementary noun (`check`, `report`) or leave it as the bare module-default action (`borg::audit::run`).
- **Apply rust.md naming rules tree-wide:** plural-s for collections (`tools()` not `list_tools()`, `entries()` not `list_blocklist()`); no underscored `.rs` filenames; first word becomes folder, second becomes file when concepts are compound (e.g., `hybrid_retrieval.rs` → `hybrid/retrieval.rs`).
- **Phase 2 completeness:** `sb status`/`sb doctor` add shared-config-drift detection and config parse-status checks.
- **Cross-doc consistency:** the prior `2026-05-19-unified-sb-binary.md` design doc gets a small update to acknowledge the `rmcp` dependency on `sb`.

### Non-Goals

- **No new features.** This is purely API surface cleanup; the user-facing CLI behavior is unchanged.
- **No new bins, no new crates.** Same workspace shape.
- **No daemon-loop or pipeline-architecture changes.** The `borg.service` and `cortex.service` daemons run the same code; only their entry-point signatures change.
- **No new dependencies.** rmcp is already pulled in; we just need to document it.
- **No changes to the CLI verb tree, flag names, or output format the user sees.** `sb borg ingest <url>` still prints the same lines in the same order. The refactor is invisible from outside.

## Proposed Solution

### Overview

Three coordinated refactors land back-to-back as one branch:

1. **Naming refactor** — drop `run_` prefix, apply plural-s for collections, rename two underscored test files. Mechanical find-and-replace + a few thought-through renames where the bare name would collide.
2. **Lib-returns-typed-data refactor** — for every print-emitting pub fn in `borg`/`cortex`/`oracle`, introduce a typed return struct that captures the data the prints conveyed; move the formatting code to `sb/src/cli/<subsystem>.rs`; sb iterates and prints.
3. **Phase 2 status/doctor completeness** — add `shared_config_findings()` and extend `config_findings()` with parse-status checks.

The lib invariant after this work: a `pub fn` in `borg`/`cortex`/`oracle` MAY only emit to stdout/stderr via `log::*` or `tracing::*` macros (which go through the logger initializer); never via `println!`/`eprintln!`/`print!`/`eprint!`. The architect (and a CI lint, see Risks) verifies this.

### Architecture

#### The lib-returns-data invariant

Codified as a first-class architectural rule of the workspace:

> **Lib crates do not write to stdout or stderr directly.** `borg`, `cortex`, `oracle` pub fns return `Result<TypedData>` (or `Result<()>` for long-running operations with no payload). `sb` is the sole consumer of those results and the sole owner of formatted output, exit codes, and process termination. Log emission via `log::*` / `tracing::*` is permitted because it routes through the logger initializer (file + stderr stream owned by `sb/src/logger.rs`), not directly to stdout.

This is what the prior design said in passing; this doc lifts it to a top-level invariant and adds a CI check.

#### Naming convention

Module path acts as the namespace; function names are bare verbs/nouns:

| Old | New |
|---|---|
| `borg::run_ingest` | `borg::ingest` |
| `borg::run_note` | `borg::note` |
| `borg::run_file_ingest` | `borg::ingest_file` |
| `borg::run_reingest` | `borg::reingest` |
| `borg::run_sign` | `borg::sign` |
| `borg::run_hotkey` | `borg::hotkey` |
| `borg::run_daemon` | `borg::daemon` |
| `borg::run_server` | `borg::serve` |
| `borg::audit::run_audit` | `borg::audit::run` (the verb is the module name; `run` reads naturally) |
| `borg::triage::run_intake_list` | `borg::triage::intake_rows` (plural-s; data, not action) |
| `borg::triage::run_intake_show` | `borg::triage::intake_row` (singular for one) |
| `borg::triage::run_dlq_list` | `borg::triage::dlq_rows` |
| `borg::triage::run_dlq_show` | `borg::triage::dlq_row` |
| `borg::triage::run_dlq_archive` | `borg::triage::dlq_archive` |
| `borg::triage::run_dlq_replay` | `borg::triage::dlq_replay` |
| `borg::triage::run_orphan_audit` | `borg::triage::orphan_audit` |
| `borg::blocklist::run_list` | `borg::blocklist::entries` (plural-s) |
| `borg::blocklist::run_remove` | `borg::blocklist::remove` |
| `borg::blocklist::run_clear` | `borg::blocklist::clear` |
| `borg::migrate::run_migrate` | `borg::migrate::run` |
| `borg::migrate::run_reingest_failed` | `borg::migrate::reingest_failed` |
| `borg::backfill::run_backfill_ingested` | `borg::backfill::ingested` |
| `borg::retention::run_sweep` | `borg::retention::sweep` |
| `borg::retention::run_status` | `borg::retention::status` |
| `borg::replay::run` | `borg::replay::run` (already correct; only the return-type needs the typed-data refactor) |
| `borg::dashboard::refresh_dashboard` | `borg::dashboard::refresh` |
| `cortex::run_lint` | `cortex::lint` |
| `cortex::run_state` | `cortex::state` |
| `cortex::run_migrate` | `cortex::migrate` (collision with module — see below) |
| `cortex::run_link` | `cortex::link` |
| `cortex::run_classify` | `cortex::classify` |
| `cortex::run_sweep` | `cortex::sweep` (collision with module — see below) |
| `cortex::run_intel` | `cortex::intel` (collision with module — see below) |
| `cortex::run_summarize` | `cortex::summarize` (collision) |
| `cortex::embed::run_embed` | `cortex::embed::run` |
| `cortex::daemon::run_daemon` | `cortex::daemon::run` |
| `oracle::run_serve` | `oracle::serve` |
| `oracle::run_index` | `oracle::index` |
| `oracle::run_call` | `oracle::call` |
| `oracle::run_list` | `oracle::tools` (plural-s, what the data IS) |
| `oracle::run_stats` | `oracle::stats` |

**Module-vs-function name collisions** (cortex's `migrate`, `sweep`, `intel`, `summarize` all have both a module and a top-level lib function with the same name).

**Initial draft assumed these were thin wrappers and proposed deleting them.** Architect review caught the error: empirically verified, `cortex::run_sweep` is **45 lines of real orchestration** (lib.rs:329-374): validates `--cold + --migrate` collision, branches on `--cold`/`--migrate`/`--proposals`/default, calls `sweep::run_cold`, `sweep::run_migrate`, `sweep::scan_proposals`, `sweep::write_proposals`. The branch logic IS the function. Similar pattern almost certainly holds for `run_state`/`run_intel`/`run_summarize` — verify each at impl time.

**Final decision:** Keep the wrappers; rename only. Resolve the collision by moving the orchestration into the matching module under a distinct entry-point name, then re-export from the crate root:

| Top-level today | Module-internal orchestrator (new home) | Crate-root alias |
|---|---|---|
| `cortex::run_lint` | (lint logic stays in lib.rs; no module collision since there's no `cortex::lint` module today) | `cortex::lint` |
| `cortex::run_classify` | (same — `classify` is a module but the top-level fn is the entry point) | `cortex::classify` (NB: today this collides with module `cortex::classify`; resolve as below) |
| `cortex::run_migrate` | `cortex::migrate::run` | `pub use migrate::run as migrate_run;` OR call `cortex::migrate::run(...)` from sb (preferred — drops the re-export cruft) |
| `cortex::run_sweep` | `cortex::sweep::run` | call `cortex::sweep::run(...)` from sb |
| `cortex::run_intel` | `cortex::intel::run` | call `cortex::intel::run(...)` from sb |
| `cortex::run_summarize` | `cortex::summarize::run` | call `cortex::summarize::run(...)` from sb |
| `cortex::run_state` | (no collision; `state` is a module but the function can be `cortex::state`) | `cortex::state` |
| `cortex::run_link` | (no collision; `linking` is the module name) | `cortex::link` |

Same rule for `cortex::classify` and `borg::audit` etc. where the obvious bare name collides with the module: move the orchestrator into the module under `run` (or another verb-noun fitting its role), and have sb call the full path.

**Concrete cortex rename plan (revised):**
- `cortex::run_lint` → `cortex::lint` (no collision; stays at crate root)
- `cortex::run_state` → `cortex::state` (no collision)
- `cortex::run_link` → `cortex::link` (no collision)
- `cortex::run_classify` → move to `cortex::classify::run` (module collision; orchestrator moves into the module)
- `cortex::run_migrate` → move to `cortex::migrate::run`
- `cortex::run_sweep` → move to `cortex::sweep::run`
- `cortex::run_intel` → already a module fn at `cortex::intel::run_intel`; rename in place to `cortex::intel::run`
- `cortex::run_summarize` → move to `cortex::summarize::run`

#### Filenames

Two test files violate `no underscores in .rs filenames`:

| Old | New |
|---|---|
| `vault/tests/regression/hybrid_retrieval.rs` | `vault/tests/regression/hybrid/retrieval.rs` |
| `vault/tests/regression/candle_parity.rs` | `vault/tests/regression/candle/parity.rs` |

Per the rust.md rule "first word becomes the module/folder, second word becomes a single-word file inside it." Decompose to subdirectories.

### Data Model

New typed return structs to introduce (in their owning lib crate):

```rust
// borg
pub struct IngestOutcome {
    pub kind: IngestOutcomeKind,
    pub title: Option<String>,
    pub path: Option<PathBuf>,
    pub trace_id: String,
}
pub enum IngestOutcomeKind { Captured, Duplicate { original_date: String }, Failed { reason: String }, Queued }

pub struct NoteOutcome { pub title: String, pub path: PathBuf }

pub struct ReingestReport {
    pub matched: usize,
    /// `dry_run = true` → only `would_process` populated; `processed` is empty.
    /// `dry_run = false` → only `processed` populated; `would_process` is empty.
    /// Disambiguates intent without forcing the consumer to cross-reference opts.
    pub would_process: Vec<ReingestCandidate>,
    pub processed: Vec<ReingestEntry>,
}
pub struct ReingestCandidate { pub date: String, pub slug: String, pub source: String }
pub struct ReingestEntry { pub source: String, pub status: ReingestEntryStatus }
pub enum ReingestEntryStatus { Replaced { title: String }, Failed { reason: String }, Other(String), Error(String) }

pub struct ServerStartup {
    pub addr: SocketAddr,
    pub telegram: SubsystemStatus,
    pub discord: SubsystemStatus,
    pub ntfy: SubsystemStatus,
    pub watchdog: SubsystemStatus,
}
pub enum SubsystemStatus { Active, SkippedNoToken, SkippedHostMismatch, Disabled }

pub struct SignResult { pub extension_dir: PathBuf, pub version: String }

pub enum DaemonOutcome {
    Installed { unit_path: PathBuf },
    Uninstalled,
    Reinstalled { unit_path: PathBuf },
    Started, Stopped, Restarted,
    Status(SystemctlStatus),
    NoAction,
}

pub enum HotkeyOutcome { Installed { key: String }, Uninstalled, NoAction }

pub struct AuditReport {
    /// All findings the audit produced. Lib emits the full Vec; sb iterates and formats.
    /// AuditFinding already exists in borg::audit and has variants MistypedContent,
    /// BlockedContent, RawUrlTitle, DuplicateNotes — keeping the existing enum preserves
    /// all current report categories without re-modeling them at the sb boundary.
    pub findings: Vec<audit::AuditFinding>,
    /// Number of findings fixed during this run. 0 when --fix was not passed.
    pub fixed_count: usize,
}

pub struct BackfillReport {
    pub scanned: usize,
    /// `dry_run = true` populates `would_backfill`; `dry_run = false` populates `backfilled`.
    pub would_backfill: usize,
    pub backfilled: usize,
    pub skipped_already_had: usize,
    pub skipped_origin: usize,
    pub skipped_recent_mtime: usize,
    pub skipped_no_date: usize,
}

// cortex
pub struct StateReport {
    pub manifest_path: PathBuf,
    pub last_scan: Option<DateTime<Utc>>,
    pub file_count: usize,
    pub diff: Option<state::Diff>, // None when --refresh without --diff
}

pub struct SweepReport {
    pub mode: SweepMode,
    pub proposals: Option<Vec<TagProposal>>, // populated when --proposals
    pub proposals_path: Option<PathBuf>,     // where proposals were written
}
/// Mirrors the Reingest/Backfill disambiguation rule: dry-run and apply
/// produce distinct enum variants so sb never has to consult input opts
/// to format output.
pub enum SweepMode {
    WouldMigrate { count: usize },
    Migrated { count: usize },
    Proposals,
    Cold { scanned: usize, surfaced: usize, pinned_excluded: usize },
}

pub struct IntelReport { pub mode: IntelMode, pub output_path: PathBuf }
pub enum IntelMode { Daily, Weekly }
```

The existing types stay: `Report` (lint/migrate/link/classify), `BackfillSummary` (summarize), `EmbedStats` (embed), `VaultStats`/`IndexStats`/`EmbeddingCoverage` (vault), `AuditHealth` (borg triage).

For `oracle::tools()` (renamed from `run_list`), the return is already typed (`Vec<rmcp::model::Tool>`).

### API Design

Every renamed function keeps its parameter list and changes only its name and return type. `sb/src/cli/<subsystem>.rs` (the callers) update to (1) use the new name and (2) format the typed return.

Example diff for `cortex::run_state` → `cortex::state`:

```rust
// before (cortex/src/lib.rs)
pub fn run_state(vault_root: &Path, config: &Config, opts: &StateOpts) -> Result<()> {
    // ... compute manifest, diff, etc ...
    println!("{}", "Added:".green().bold());
    for p in &diff.added { println!("  + {}", p.display()); }
    // ... more printlns ...
    Ok(())
}

// after (cortex/src/lib.rs)
pub fn state(vault_root: &Path, config: &Config, opts: &StateOpts) -> Result<StateReport> {
    // ... compute manifest, diff, etc ...
    Ok(StateReport { manifest_path, last_scan, file_count, diff: Some(diff) })
}

// sb/src/cli/cortex.rs
Command::State(a) => {
    let report = cortex::state(&vault_root, &config, &a.into())?;
    print_state_report(&report);
}

// new helper in sb/src/cli/cortex.rs
fn print_state_report(r: &cortex::StateReport) {
    if let Some(diff) = &r.diff {
        if !diff.added.is_empty() {
            println!("{}", "Added:".green().bold());
            for p in &diff.added { println!("  + {}", p.display()); }
        }
        // ... etc ...
    }
}
```

### Implementation Plan

The phases ship back-to-back as one branch per `feedback-no-phase-gating`. Each phase commits independently with otto ci green; the branch merges as one bump.

#### Phase 1: Naming refactor (drop `run_` prefix, apply plural-s, rename test files)
**Model:** sonnet

- For every lib pub fn listed in the rename table above, perform the rename. Update all call sites in `sb/src/cli/<subsystem>.rs` and any cortex/borg-internal callers (e.g., `cortex::daemon::run_daemon` calls into `run_intel`, `run_lint`, etc.).
- For cortex's module-vs-function collisions (`migrate`, `sweep`, `intel`, `summarize`): drop the top-level wrapper, replace `cortex::run_lint(...)` call sites with `cortex::lint::run(...)` (or whichever path is the actual implementation entry point). Check if the existing lib.rs functions are wrappers (likely yes) and collapse them.
- Test-file decomposition (per rust.md: "no underscores in .rs filenames; first word becomes the module/folder, second becomes the file"):
  - `git mv vault/tests/regression/hybrid_retrieval.rs vault/tests/regression/hybrid/retrieval.rs`. Create `vault/tests/regression/hybrid.rs` (the Rust-2018-style module entry) declaring `pub mod retrieval;`. Update wherever `regression` is wired (look for `mod hybrid_retrieval` or `mod regression::hybrid_retrieval`) to use the new path.
  - Same for `candle_parity.rs` → `candle/parity.rs` + new `candle.rs` entry with `pub mod parity;`.
  - `cargo test --workspace` must still discover and run both tests after the move.
- Gate: `otto ci` passes. All tests still pass.

#### Phase 2: Lib-returns-data refactor — cortex remaining functions
**Model:** sonnet

- `cortex::state` (was `run_state`): introduce `StateReport`; remove the diff prints and "No changes since last scan." messages; sb prints.
- `cortex::sweep` (was `run_sweep`): introduce `SweepReport`; remove the 7 printlns at branch terminals; sb prints based on `SweepReport.mode` and `count`/`proposals`.
- `cortex::intel::run` (was `intel::run_intel`): introduce `IntelReport`; remove 2 printlns; sb prints.
- `cortex::embed::run` (was `embed::run_embed`): replace 2 printlns; `EmbedStats` already exists, so signature stays `Result<EmbedStats>`. sb takes the existing log::info! summary line and converts to println for the user-visible "complete" message. Remove the prefetch-only println; sb prints "Prefetched embedding model {model}" before/after the lib call.
- `cortex::daemon::run` (was `daemon::run_daemon`): scan for any printlns the long-running daemon emits; convert to `log::info!` (the daemon should not be writing to stdout — it runs under systemd which captures journald).
- Gate: `otto ci` passes; smoke-test `sb cortex state --diff`, `sb cortex sweep --proposals`, `sb cortex intel --help`, `sb cortex embed --prefetch-model` produce the same human output as before.

#### Phase 3: Lib-returns-data refactor — borg lib.rs handlers
**Model:** opus

This is the largest single phase. borg/src/lib.rs has 100+ printlns across ~12 pub fns. Work through them in dependency order:

- `borg::sign` (was `run_sign`): introduce `SignResult { extension_dir, version }`. Remove 2 printlns. sb formats "Signing extension v{version} in {dir}" and "Extension signed successfully" lines.
- `borg::hotkey` (was `run_hotkey`): introduce `HotkeyOutcome`. Remove "No hotkey action specified" eprintln; sb maps `HotkeyOutcome::NoAction` to an error or help message.
- `borg::daemon` (was `run_daemon`): introduce `DaemonOutcome`. The various branch outcomes (install/uninstall/start/stop/restart/status/no-action) map to enum variants. Remove printlns in `install_systemd`/`uninstall_systemd`/`install_launchd`/`uninstall_launchd`/`stop_service`/`restart_service`/`show_status`. These helpers return their respective data; sb prints "Wrote {path}", "Service installed and started.", "Removed {path}", etc. (Critically: `install_systemd` already writes the unit file; the println about "Wrote ..." just acknowledges the side effect. Move the acknowledgement to sb.)
- `borg::serve` (was `run_server`): introduce `ServerStartup { addr, telegram, discord, ntfy, watchdog }` with `SubsystemStatus` variants. The bewildering startup banner ("--> telegram bot active", "--> http server on {addr}", "--> watchdog active") becomes structured. Verified during Pass 2: `run_server` spawns all subsystems into a `tokio::task::JoinSet` then `joinset.join_next().await` blocks until a task exits (which is "never" under normal operation).

  **Architect-revised API (don't leak `JoinSet` across the lib boundary):**
  ```rust
  /// Boots all subsystems and returns the startup banner data + an opaque handle.
  pub async fn serve_init(config: Config) -> Result<(ServerStartup, ServerHandle)>;

  /// Opaque wrapper around the internal tokio::task::JoinSet. sb has no
  /// dependency on the concrete tokio concurrency primitive.
  pub struct ServerHandle { /* private tokio::task::JoinSet<Result<(), eyre::Error>> */ }
  impl ServerHandle {
      /// Awaits any of the spawned tasks to exit. Under normal operation this never returns.
      pub async fn wait(self) -> Result<()>;
  }
  ```

  sb:
  ```rust
  let (startup, handle) = borg::serve_init(config).await?;
  print_server_banner(&startup);
  handle.wait().await
  ```

  The opaque `ServerHandle` keeps tokio's `JoinSet<Result<(), eyre::Error>>` an internal implementation detail of `borg`; sb sees only the `.wait()` method. This is the Architect's preferred shape over the initial draft's `Result<(ServerStartup, JoinSet)>` proposal, which leaked the concurrency primitive.
- `borg::note` (was `run_note`): introduce `NoteOutcome`. Two printlns ("Captured: ... -> path" or "Error: {reason}") become enum variants on `IngestOutcome::Captured` / `IngestOutcome::Failed`. sb prints.
- `borg::ingest` and `borg::ingest_file` (were `run_ingest` and `run_file_ingest`): both return `IngestOutcome`. Captured/Duplicate/Failed/Queued variants captured. sb prints "Captured:", "Duplicate: already ingested on {date}", "Error: {reason}", "Queued for processing." based on variant.
- `borg::reingest` (was `run_reingest`): introduce `ReingestReport`. The current "No matching entries found.", "Reingest complete.", per-entry "    -> Replaced: ", "    -> Failed: ", "    -> Error: " prints become structured. sb iterates `report.processed` and prints. NOTE: this is one of the "iterative print" cases the architect identified as a real tradeoff. Decision: buffer (return Vec<ReingestEntry>); memory cost is bounded by ledger size (Scott's vault is ~800 rows = ~70KB). For very large vaults this might warrant a channel/callback pattern, but not at current scale.
- Gate: `otto ci` passes; smoke-test each user-visible verb produces the same output.
- **Mandatory live smoke test (per Architect's specific prediction):** run `sb borg reingest --all --type youtube --before 2026-05-01 --dry-run` against the live ledger and **visually confirm progress lines stream as entries are visited** — NOT all-at-once at the end. This is the test the Architect predicted I would skip while testing buffering against `audit --fix` (fast disk I/O). The streaming verification is non-negotiable for Phase 3 to be declared done.

#### Phase 4: Lib-returns-data refactor — borg sub-modules
**Model:** opus

- `borg::audit::run` (was `run_audit`): introduce `AuditReport { duplicates, fixed_count }`. Remove "[DUPLICATE] ... -> N notes found" iterative prints; sb iterates `report.duplicates`. The `--fix` branch is the actual mutation case; lib returns `fixed_count`, sb confirms with the user via printed summary. (The architect agreed this is genuinely iterative for `--fix` because each fix is a real I/O event, but the duplicate-finding side is pre-computed and prints over the gathered map — same as cortex sweep.)
- `borg::triage::intake_rows` (was `run_intake_list`): change return from `Result<()>` to `Result<Vec<IntakeRow>>`. sb formats the table header + rows.
- `borg::triage::intake_row` (was `run_intake_show`): return `Result<IntakeRowDetail>` with the row + sidecar contents. sb prints the multi-line breakdown.
- `borg::triage::dlq_rows` / `dlq_row` / `dlq_archive` / `dlq_replay`: same pattern; return typed data; sb formats.
- `borg::triage::orphan_audit` (was `run_orphan_audit`): return `Result<OrphanAuditReport { orphan_count, oldest_age, report_path }>`. The "writes `system/views/borg-orphans.md`" side effect stays in the lib (it's the actual work); the "wrote N orphans" announcement moves to sb.
- `borg::migrate::run` (was `run_migrate`): introduce `MigrateReport`. Apply path / dry-run summary lines move to sb.
- `borg::migrate::reingest_failed` (was `run_reingest_failed`): introduce `ReingestFailedReport { matched: Vec<PathBuf>, apply: bool }`. sb prints "[dry-run] reingest-failed: N matching note(s)" and the list.
- `borg::backfill::ingested` (was `run_backfill_ingested`): introduce `BackfillReport`. The detailed summary at the end ("scanned: 1236", "backfilled: 115 (dry-run)", etc.) moves to sb.
- `borg::retention::sweep` / `borg::retention::status`: return `RetentionReport { traces, rejected, disk_bytes, staging_root }`. sb prints.
- `borg::replay::run`: scan for printlns; introduce `ReplayReport`. Same iterative-print concern as reingest; buffer.
- `borg::dashboard::refresh`: already uses `log::info!`, not `println!` per the architect. Just rename.
- Gate: `otto ci` passes; smoke-test the full Phase 2 surface (sb borg audit, intake list/show, dlq list/show, reingest --dry-run, replay --dry-run, retention status, etc.).

#### Phase 5: Phase 2 missed mandates
**Model:** sonnet

- `sb/src/cli/checks.rs`: extend `config_findings()` to also try `serde_yaml::from_str::<<subsystem>::Config>(...)` on each present config file. If parse fails, add a Finding::error with the parse error message and `sb bootstrap` (or hand-edit hint) as the suggested fix. The existing `path.exists()` check stays as the first gate.
- `sb/src/cli/checks.rs`: add a new `shared_config_findings()` Section. Compare `config/{canonical-tags,tag-mapping,tag-proposals}.yml` in the repo (when running from repo root) against `~/.config/second-brain/*.yml`. Same hash-compare pattern as `pattern_findings()`. Report drift count or "in sync".
- Wire both into `all_sections()`.
- Gate: smoke-test `sb status` and `sb doctor` show the new sections.

#### Phase 6: Cross-doc consistency
**Model:** sonnet

- Update `docs/design/2026-05-19-unified-sb-binary.md` to acknowledge `rmcp = "1.3.0"` as an `sb` crate dependency (the prior doc's dependency list says "No new external dependencies"; that was true at the time of writing but became false during Phase C).
- Update CLAUDE.md if there's any line referencing `run_*` functions or old API names.
- Update the shakedown report (`docs/shakedown-sb-v0.8.2.md`) if it contains pipeline examples using old function paths — actually those were CLI-surface examples (e.g., `sb oracle call`), not lib calls, so likely unaffected.

#### Phase 7: CI guard against regression
**Model:** sonnet

Use Rust's native AST-aware lints (the Architect's recommendation; replaces the initial grep-based approach which would have been broken because `grep -v '#[cfg(test)]'` strips only the line containing the attribute, not the test module body — confirmed by inspection that `borg/src/pipeline.rs`, `youtube.rs`, etc. contain `println!`s inside `#[cfg(test)] mod tests` that grep would falsely flag).

- Add to the top of `borg/src/lib.rs`, `cortex/src/lib.rs`, `oracle/src/lib.rs`:
  ```rust
  #![deny(clippy::print_stdout, clippy::print_stderr)]
  ```
- For test modules that legitimately use `println!` (test fixtures, helpers that print captured output), allow at the module level — either as a separate `#![allow(...)]` inside the test file (Rust-2018 `tests.rs` style which this codebase uses) or via `#[cfg_attr(test, allow(clippy::print_stdout, clippy::print_stderr))]` on the inline `#[cfg(test)] mod tests;` declaration.
- `otto ci` runs `cargo clippy --workspace --all-targets --features vec -- -D warnings` (already does); the new `deny` attributes turn print-stmt usage into a hard CI failure.

Without this lint, the next refactor that adds a "quick println for debugging" silently undoes Phase 2/3/4.

## Alternatives Considered

### Alternative 1: Leave the debt; document it in the prior design doc

- **Description:** Accept the 154 borg + 69 cortex println sites as "intentionally deferred mechanical debt." Add an "Accepted technical debt" section to the original `unified-sb-binary.md` design doc listing every print-emitting function. No code changes.
- **Pros:** Zero code-change risk. Cutover can proceed immediately. The CLI surface works correctly; the only thing wrong is the internal API contract.
- **Cons:** Scott explicitly rejected this option ("we are not leaving any 'documented debt'"). Also: even documented, it tends to rot — the next reader sees the print statements and concludes the lib boundary doesn't matter, and a print creeps into a NEW pub fn. The CI guard in Phase 7 makes the principle defensible long-term.
- **Why not chosen:** Direct user rejection.

### Alternative 2: Keep `run_` prefix; only fix the print debt

- **Description:** Do Phases 2-5 but skip Phase 1 (the rename). Lib fns stay `run_*` named but return typed data.
- **Pros:** Lower churn — Phase 1 touches every call site across `sb/src/cli/`. Skipping it removes ~50 mechanical sed-style edits.
- **Cons:** Scott explicitly flagged the `run_` prefix as annoying. The double-namespacing (`borg::triage::run_dlq_list`) is genuinely worse than `borg::triage::dlq_rows`. And the rename pairs naturally with the print refactor — both touch the lib API contract, so doing them together amortizes the call-site churn.
- **Why not chosen:** Direct user feedback; the consolidation is cheaper than doing them sequentially.

### Alternative 3: Callback/channel pattern for live progress

- **Description:** For the iterative-print fns, use a `progress: impl FnMut(&ProgressEvent)` callback so sb prints live as the lib processes.
- **Pros:** Zero memory cost; live UX (user sees progress as items process; doesn't have to wait for the full operation to complete).
- **Cons:** Adds a parameter to every iterative pub fn.
- **Decision (split):** Pure-buffer was the initial pick; **the Architect's design review flipped that choice for I/O-bound iterative handlers.** Verified empirically: `borg::run_reingest` makes one `client.post(&endpoint).json(&body).send().await` per ledger entry (lib.rs:514). At 800 entries that's 800 sequential HTTP calls; buffering the output would silence the CLI for 10+ minutes with no feedback — an unacceptable UX regression. Same risk for any handler that does network or disk I/O inside its iteration loop.

  Final rule, applied per-function:
  | Function | Inner-loop shape | Pattern |
  |---|---|---|
  | `borg::reingest` | sequential HTTP per entry (`client.post().await`) | **callback** |
  | `borg::replay::run` | sequential HTTP per trace (verify during impl) | **callback** |
  | `borg::audit::run` w/ `--fix` | sequential disk I/O per fix | **callback** |
  | `borg::migrate::run` w/ `--apply` | rayon `par_iter`, buffers results | buffer |
  | `borg::backfill::ingested` w/ `--apply` | rayon `par_iter`, buffers results | buffer |
  | `borg::audit::run` (no `--fix`, just listing) | in-memory hashmap iteration | buffer |
  | `borg::blocklist::entries` | already in-memory | buffer |
  | `borg::triage::{intake_rows, dlq_rows}` | already in-memory file parse | buffer |
  | every Report-returning cortex fn (`lint`, `migrate`, …) | in-memory after vault scan | buffer |

  Note on `migrate --apply` and `backfill --apply`: empirically verified these use `rayon::par_iter` (`borg/src/migrate.rs:176`, `:334`; `borg/src/backfill.rs:174`) and already collect into a `Vec` before any output. A callback passed to a rayon worker pool would need `Send + Sync` bounds, mutex-guarded state, or a channel — none of which is justified when the function already buffers internally. They join the buffer column.

  **Canonical callback signature** for the genuinely sequential HTTP cases (sync closure required to be `Send` since the async fn holds it across `.await` points; without `Send` the returned `Future` becomes non-`Send`, which breaks if anything ever wraps the call in `tokio::spawn`):

  ```rust
  pub async fn reingest(
      config: Config,
      opts: ReingestOpts,
      mut progress: impl FnMut(&ReingestEvent) + Send,
  ) -> Result<ReingestReport> { ... }

  pub enum ReingestEvent {
      Matched { count: usize, dry_run: bool },
      ItemStart { index: usize, total: usize, date: String, slug: String, source: String },
      ItemReplaced { title: String },
      ItemFailed { reason: String },
      ItemOther(String),
      ItemError(String),
  }
  ```

  sb's caller in `sb/src/cli/borg.rs` constructs the same human output it used to print directly:

  ```rust
  let report = borg::reingest(config, opts, |event| match event {
      ReingestEvent::Matched { count, dry_run } => println!(
          "{} {} entries{}", if *dry_run { "Would reingest" } else { "Reingesting" },
          count, if *dry_run { " (dry run)" } else { "" }
      ),
      ReingestEvent::ItemStart { index, total, date, slug, source } =>
          println!("  [{}/{}] {} - {} ({})", index + 1, total, date, slug, source),
      ReingestEvent::ItemReplaced { title } => println!("    -> Replaced: \"{title}\""),
      // ...
  }).await?;
  ```

  Memory cost stays low (events are small structs, allocated on stack). Live UX is preserved.
- **Why pure-buffer was the wrong default:** I evaluated only the memory dimension and ignored time/UX. The Architect's audit caught it. The split-by-I/O-shape decision above honors both: in-memory iterations remain simple buffers; I/O-bound iterations get the callback so users see progress as it happens.

### Alternative 4: Lift typed structs to a shared `sb-types` crate

- **Description:** All the new return structs (`IngestOutcome`, `AuditReport`, `BackfillReport`, etc.) live in a new `sb-types` crate that borg/cortex/oracle and sb all depend on.
- **Pros:** Single source of truth for cross-crate types; avoids re-exports.
- **Cons:** Adds another crate to the workspace. Types belong with the code that produces them; an `IngestOutcome` makes no sense outside of borg's pipeline. Re-exporting from sb is unnecessary because sb depends on all three sibling crates directly.
- **Why not chosen:** Premature abstraction. Each lib owns its return types; sb consumes them via direct path imports.

## Technical Considerations

### Dependencies

No new dependencies. `serde_yaml` (for the new config-parse check in Phase 5) is already in the workspace.

### Performance

The refactor introduces typed structs allocated on the heap where there were previously direct prints. For commands that produce small reports (`borg::sign`, `cortex::lint`'s 3833 violations, etc.), allocation cost is negligible against the ~seconds it takes to scan the vault. For `borg::reingest`'s ~800 rows, the buffered `Vec<ReingestEntry>` is ~150KB and serializes/prints in milliseconds. No measurable performance regression.

The `sb status` parse-status check adds N (= 3) YAML parses to every invocation. Each parse is microseconds; total adds nothing perceptible to the existing systemctl shell-out cost.

### Security

No new attack surface. Lib functions returning typed data are strictly less powerful than functions that write directly to stdout — they can't print sensitive data to a terminal a user didn't expect. Logging continues to flow through the `vault::logging` writer chain (file + stderr).

### Testing Strategy

- **Existing tests continue to pass.** Most lib tests exercise the underlying logic, not the print output; the rename + return-type change doesn't break them. The few tests that might inspect printed output (if any) need updating to inspect the returned struct instead.
- **Add new unit tests for each new return struct.** Pure construction tests — given an input, does the lib produce the right typed output? These cover the same ground as the previous "did it print the right line" assertions, but with structural assertions.
- **Smoke tests in `otto ci`:** `sb borg --help`, `sb cortex --help`, `sb oracle --help`, plus one read-only command per subsystem (`sb borg blocklist list`, `sb cortex lint --rule frontmatter --format json | jq .`, `sb oracle stats`) to confirm the renames didn't break clap dispatch or output format.
- **Manual smoke test of Phase 5 additions:** `sb status` should show new sections; `sb doctor` should sort the new findings correctly.

### Rollout Plan

Single branch (`lib-api-cleanup` or extend the existing `unified-sb-binary` branch if it hasn't been merged yet — preferred, since this is a continuation of the same work).

1. Phases 1-7 land as a sequence of commits.
2. `otto ci` green at each phase boundary.
3. `bump` (patch — the CLI surface is unchanged, so even though the lib API is overhauled, there are no external consumers of these crates to break).
4. `git push && git push --tags`.
5. `otto deploy` (restarts daemons under the new binary; the runtime behavior is identical).

No data is at risk (no on-disk format changes, no SQLite schema changes).

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| Phase 3 (borg lib.rs) is large enough that mid-phase commits leave a broken main; or that the typed-outcome refactor for `run_server` proves harder than expected (event-loop integration) | Medium | Medium | Each function refactored is its own commit; `otto ci` green between them. If `run_server` integration is uniquely hard, defer it to its own follow-up phase and document the deferral inline (acceptable because run_server's prints are startup-banner only, not iterative — a daemon process under systemd writes those once and they go to journald regardless). |
| The CI lint in Phase 7 has false positives (e.g., string literals containing `println!`, doc comments, test-only code that doesn't have a clean `#[cfg(test)]` boundary) | Medium | Low | Tune the lint to skip `tests/` directories and `#[cfg(test)]` blocks; allow an in-source `// allow-println` marker for the rare legitimate exception (e.g., embedded help text in a string literal that happens to contain `println!`). |
| Renames break call sites in unexpected places (e.g., `borg::triage::run_dlq_list` is invoked from cortex's daemon or a third caller I didn't enumerate) | Low | Low | `cargo check --workspace` after each rename catches missed call sites; the compiler IS the safety net for mechanical renames. |
| Some lib fns currently have multiple control paths (apply vs dry-run vs lint) that produce wildly different print shapes, making a single typed return awkward | Medium | Medium | Use enums for outcome-shape variation (`enum SweepMode { Migrate { dry_run: bool }, Proposals, Cold }`); each variant carries the data its mode needs. This is already the pattern used for `IngestOutcome`. |
| The cortex collision-resolution (drop top-level wrappers like `cortex::run_migrate`) might surface that the wrappers DID contain real logic, not just delegation | Low | Medium | Before deleting any wrapper, read its body; if it's >5 lines or has its own state, keep the wrapper but rename it (e.g., `cortex::run_migrate` -> `cortex::migrate_all` or similar). Don't blindly delete. |
| `oracle::tools()` (renamed from `run_list`) returns `Vec<rmcp::model::Tool>`, which forces consumers to depend on rmcp. sb already does; no other consumer should | Low | Low | Acceptable — rmcp::Tool IS the canonical type. Adding a thin shim would just hide it. |
| Phase 7's CI lint trips up on legitimate use of `println!` somewhere I haven't anticipated (e.g., test fixtures that print captured stdout, build.rs scripts) | Low | Low | Scope the grep to `borg/src/`, `cortex/src/`, `oracle/src/` proper; exclude `tests/`, `examples/`, `benches/`, `build.rs`. Keep the lint check tight. |
| The two test file renames (`hybrid_retrieval.rs`, `candle_parity.rs`) break the regression test discovery if the test harness wires file paths explicitly | Low | Low | Cargo's test discovery is path-based — moving a file under `tests/regression/` to `tests/regression/hybrid/` just requires updating the `mod` declarations. Verify with `cargo test --workspace`. |

## Open Questions

- [x] **`borg::serve` event-loop integration:** Resolved during architect review. Lib exposes `serve_init(config) -> Result<(ServerStartup, ServerHandle)>` where `ServerHandle` is an opaque wrapper around the internal `tokio::task::JoinSet`. sb prints the banner from `ServerStartup`, then calls `handle.wait().await` to block on the daemon. The concrete tokio primitive does not leak across the lib boundary. See Phase 3 detail for `borg::serve` and the architect-revised API block.
- [x] **Should `run_*` wrappers in cortex be deleted or kept?** Resolved: KEEP the wrappers, only rename. Architect review caught that `cortex::run_sweep` is 45 lines of real orchestration (collision-validation + branch dispatch into `sweep::run_cold`/`run_migrate`/`scan_proposals`/`write_proposals`), not a thin delegation. The orchestration moves INTO each matching module under a distinct entry point (`cortex::sweep::run`, `cortex::migrate::run`, etc.), and sb calls the full path. The initial "delete the wrappers" framing was wrong; see the revised collision-resolution table in `### Architecture`.
- [x] **Is the Phase 7 CI lint worth its complexity?** Resolved: ship the lint, but via crate-level `#![deny(clippy::print_stdout, clippy::print_stderr)]` at the top of `borg/src/lib.rs`, `cortex/src/lib.rs`, `oracle/src/lib.rs`, NOT via shell-based grep in `.otto.yml`. Rust's native AST-aware lints distinguish real `println!` calls from string literals and respect `#[cfg(test)]` boundaries (grep cannot). Test modules that legitimately print get a scoped `#[cfg_attr(test, allow(...))]` or per-module `#![allow(...)]`. See Phase 7 detail.

## References

- Prior design doc: `docs/design/2026-05-19-unified-sb-binary.md` (the work this doc completes)
- Architect implementation audit (in-conversation, 2026-05-19): identified the 6+2 gaps this doc addresses
- Shakedown report: `docs/shakedown-sb-v0.8.2.md`
- Rust conventions: `~/repos/scottidler/claude/HOME/repos/.claude/rules/rust.md` (filename, naming, shell/core split rules this doc applies)
- Memory: `feedback-no-phase-gating`, `feedback-self-contained`, `feedback-named-columns`, `feedback-no-single-use-bindings`
