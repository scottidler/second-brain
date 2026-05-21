# Design Document: borg receipts log + success-only markdown surface

**Author:** Scott Idler
**Date:** 2026-05-20
**Status:** Implemented (Phase 5 migration shipped as `bin/migrate-receipts` bash script rather than the Rust verb originally specified)
**Review Passes Completed:** 6/5 + Architect rounds 1, 2, 3 (all findings absorbed)

## Summary

Borg today maintains six markdown files for ingestion bookkeeping (`borg-intake.md`, `borg-ledger.md`, `borg-dlq.md`, `borg-dlq-archive.md`, `borg-orphans.md`, `borg-dashboard.md`) that conflate three unrelated concerns (durability, audit, user-facing browsing) and leave most of the `DlqStage` enum wired-but-unused. Collapse the bookkeeping into two layers: a durable receipts log in a borg-owned SQLite database at `~/.local/share/sb/borg/receipts.db` that holds every input ever delivered with full status + failure-stage taxonomy, and a success-only `borg-ledger.md` plus the existing `borg-dashboard.md` in the vault. The four extra markdown files go away; the dead `DlqStage` variants get wired to actual pipeline error sites; the watchdog stops parsing markdown tables and runs as an in-process scan that consults the live `permits::is_trace_active` set before issuing targeted `UPDATE`s.

## TL;DR

- **Two stores, two layers.** Durable receipts SQLite at `~/.local/share/sb/borg/receipts.db` (every input ever, with status + failure_stage). Obsidian-facing markdown: `borg-ledger.md` (success-only) + `borg-dashboard.md` (unchanged).
- **Four vault files deleted:** `borg-intake.md`, `borg-dlq.md`, `borg-dlq-archive.md`, `borg-orphans.md`. The vault stays a clean record of what got published.
- **`DlqStage` finally wired.** Every pipeline error site classifies its failure (fetch-failed, quality-blocked, pipeline-timed-out, publish-failed, classify-failed, intake-rejected, crashed). The seven-variant enum stops being dead code; the `PipelineError` wrapper type enforces this at compile time.
- **No retry semantics.** Failures are terminal on first attempt. `sb borg replay` exists for manual reinjection.
- **Watchdog is permit-aware, not pure SQL.** It scans `status='received' AND received_at < deadline`, then filters in-memory through `permits::is_trace_active` before issuing targeted `UPDATE`s. This preserves the existing behavior where a trace can legitimately sit in the `HEAVY_PERMITS` queue for hours during bulk uploads without being declared crashed.
- **One CLAUDE.md amendment.** "Borg may own its own SQLite for the receipts log. Oracle still owns its own separate FTS5+vector SQLite. Different files, different writers."

## Problem Statement

### Background

Borg started with two markdown files in the vault: `borg-ledger.md` (a flat chronological table of items ingested, statically rendered) and `borg-dashboard.md` (a Dataview-driven browseable view over published notes). These matched the user's mental model: a chronological input record and a vault-discovery surface. Both were Obsidian-native.

Over time, three concerns drove the addition of four more files:

1. **Durability across crashes.** The pipeline can die mid-flight (OOM, panic, manual kill). The fix landed as `borg-intake.md`: a synchronous-at-the-door append-only log written before any pipeline work, plus a background `borg::watchdog` that scans `borg-intake.md` every 60 seconds and writes a `watchdog-orphan` DLQ row for any trace older than `pipeline.hard_timeout_secs + 60s` that never produced a terminal row.

2. **Failure classification.** The `vault::dlq::DlqStage` enum was introduced with seven variants (`IntakeReject`, `ClassifyFailed`, `FetchFailed`, `QualityBlocked`, `PipelineTimedOut`, `PublishFailed`, `WatchdogOrphan`) and a per-stage `reason` column on `borg-dlq.md`, intended to let the operator distinguish "yt-dlp couldn't reach YouTube" from "fabric returned a block page" from "the quality gate refused the produced note." Only two of the seven variants were ever wired into a write path: `IntakeReject` at the front doors (`routes.rs`, `telegram.rs`, `discord.rs`, `ntfy.rs`) and `WatchdogOrphan` in the watchdog. The other five variants exist in the enum but are never written.

3. **Triage ergonomics.** `borg-dlq-archive.md` was added as a sweep target for resolved DLQ rows so the active DLQ stayed short. `borg-orphans.md` was added as a Dataview-friendly materialization of the watchdog's findings (Dataview cannot join two tables, so the orphan set has to be materialized rather than queried).

### Problem

The accretion produces six concrete problems for the operator and one for the codebase:

- **Pipeline failures do not appear in the DLQ.** `borg::pipeline::make_failure` (`borg/src/pipeline.rs:276-311`) writes a `LedgerStatus::Failed` row to `borg-ledger.md` and returns. No DLQ row is written. So when the operator opens `borg-dlq.md` to see "what failed and why," all they find are watchdog-orphan rows whose `reason` is the generic string "no ledger or dlq row produced within 1860s." The actual failure (`yt-dlp failed: Sign in to confirm you're not a bot`, `Content quality check failed: Blocked content detected in title: 'Just a moment...'`) is only in the rotating borg log file.

- **The dead-code DlqStage variants represent lost diagnostic value.** Today's failure classification surface in markdown is "succeeded vs failed." The taxonomy needed to distinguish fetch failures from quality-gate refusals from publish failures already exists in the enum and is durable enough to act on (cookies-from-browser for fetch-failed against YouTube, fallback fetcher for quality-blocked against Cloudflare interstitials, retry on publish-failed). Without it wired up, every failure looks the same.

- **The DLQ conflates three unrelated concerns in one file.** `IntakeReject` is a front-door validation failure, fundamentally different from `WatchdogOrphan` (a crash) or the never-written `FetchFailed`/`QualityBlocked` (genuine pipeline failures). The single "DLQ" surface forces the operator to mentally re-sort rows by stage on every read.

- **The watchdog-archive interaction is a death spiral.** `archive --resolved` moves DLQ rows to `borg-dlq-archive.md`. The watchdog's invariant check is `intake_trace IN (ledger OR dlq)`. Archiving the trace breaks the invariant, so the next 60-second scan re-creates the DLQ row. Today the operator's mental fix is "mark resolved without bulk-sweeping," which works but is non-obvious and easy to get wrong.

- **`borg-intake.md` exposes implementation detail to the user.** The user never reads or queries it; it exists solely so the watchdog can detect orphans. It is a daemon-internal data structure rendered as a vault file. It clutters the vault and risks accidental edits.

- **The markdown table format scales poorly as the system of record.** `borg-intake.md` is currently 19K, `borg-ledger.md` is 208K. Every append takes a file lock, every read parses the entire table. SQLite is the customary tool for an append-mostly log with per-row mutable status and indexed queries; markdown table parsing is a workaround for not having one.

- **The CLAUDE.md invariant statement is imprecise.** The current wording, "every `trace_id` in `borg-intake.md` must also appear in `borg-ledger.md` (success path) or `borg-dlq.md` (failure path)," implies failures live in the DLQ. They live in the ledger with `❌`. The doc text misled at least one investigator (this one) into proposing a non-bug as a bug.

### Goals

- **Never lose input.** Every input delivered through any front door (HTTP, Telegram, Discord, ntfy, CLI) is durably recorded at the moment of receipt, before any pipeline work runs.
- **Classify every failure by stage.** Each terminal failure carries one of the existing `DlqStage` values (`intake-rejected`, `classify-failed`, `fetch-failed`, `quality-blocked`, `pipeline-timed-out`, `publish-failed`, `crashed`). The `DlqStage` enum is wired into every pipeline error site that can produce it.
- **Only successes appear in Obsidian markdown.** Failures live in the durable receipts log only; they are queryable via CLI but not visible in the vault. The vault stays a clean record of what got published.
- **Two markdown files in the vault, scope-split:** `borg-ledger.md` (append-only static table of every successful ingestion, raw URL primary) and `borg-dashboard.md` (Dataview-windowed browseable view over published notes, unchanged).
- **The watchdog stops parsing markdown.** Replaces the current `borg-intake.md` + `borg-dlq.md` round-trip with a `SELECT trace_id FROM receipts WHERE status='received' AND received_at < (now - deadline)` followed by an in-memory filter through `permits::is_trace_active` and one targeted `UPDATE` per surviving orphan. The permit-aware filter is preserved (a trace queued for hours in `HEAVY_PERMITS` is not yet crashed); only the markdown parsing dies.
- **CLI surface for the invisible failures.** `sb borg log`, `sb borg show <trace>`, `sb borg replay <trace>` are the operator's window into the receipts log. The existing `sb borg intake/dlq/audit` surface is reshaped to read from the receipts log instead of from markdown.
- **Documented architectural amendment.** The CLAUDE.md rule "borg does NOT depend on rusqlite" is updated to reflect the new boundary: oracle owns its FTS5 SQLite database for indexing; borg owns its own SQLite database for the receipts log. They are separate files, separate concerns, separate writers.

### Non-Goals

- **No retry semantics.** Failures are terminal on first attempt. The `DlqStatus::Retried` variant goes away; pipeline retries are out of scope. If a fetch fails, it stays failed until the operator replays it. (`sb borg replay` re-injects the raw input as a new trace.)
- **No reshape of the dashboard.** `borg-dashboard.md` stays exactly as it is: Dataview windowed queries over published notes, no Dataview perf regressions, no change to the user's vault-discovery workflow.
- **No change to the published note frontmatter.** `source`, `method`, `ingested`, `date`, `domain`, `type`, etc. all stay. The Dataview queries on the dashboard read these fields and do not need to change.
- **No change to oracle's FTS5+vector store.** Oracle still owns its existing SQLite database at its existing path. Borg's receipts SQLite is a separate file the oracle process never opens. (Oracle's `ingest_history` MCP tool continues to query `borg-ledger.md` and is extended with a sibling `failure_history` tool that queries the receipts DB; see Phase 3.)
- **No change to the staged-ingestion pipeline.** The `system/intake/<trace>.txt` raw-input sidecars (per the 2026-04-19 staged-pipeline design) remain as they are; they are the binary-safe storage for replay payloads. The receipts log references the trace_id, the sidecar holds the bytes.
- **No reshape of `borg-ledger.md`'s in-Obsidian rendering.** The ledger stays a static markdown table, not a Dataview block. This is intentional: it is the chronology-friendly grep surface, not the interactive surface.

## Proposed Solution

### Overview

Two layers of state with crisp scope:

```
Layer 1 (durable, never user-facing):
  ~/.local/share/sb/borg/receipts.db   -- SQLite, one row per trace_id
    status ∈ { received, succeeded, failed }
    failure_stage ∈ { intake-rejected, classify-failed, fetch-failed,
                      quality-blocked, pipeline-timed-out, publish-failed,
                      crashed }
    Written at the door (status=received) and updated in place at terminal
    time. Watchdog promotes stale received rows to failed/crashed.

Layer 2 (in the Obsidian vault, success-only):
  system/views/borg-ledger.md          -- append-only static table.
                                         One row per successful ingestion.
                                         Raw URL primary.
  system/views/borg-dashboard.md       -- unchanged. Dataview over notes.

Removed from the vault:
  system/views/borg-intake.md          -- moves to Layer 1
  system/views/borg-dlq.md             -- moves to Layer 1 (as status=failed)
  system/views/borg-dlq-archive.md     -- no concept; failures just stay in L1
  system/views/borg-orphans.md         -- no concept; query L1 directly
```

The pipeline's success path writes both layers (UPDATE L1 to `succeeded` + APPEND L2). The pipeline's failure path writes only L1 (UPDATE L1 to `failed` with stage + reason). The watchdog writes only L1.

### State machine

Every trace_id moves through exactly this state machine in the receipts DB:

```
                                       success path
                                    +-------------------> [succeeded]
                                    |  (UPDATE: status,
                                    |   terminal_at, note_path)
                                    |
[start] --received--> [received] ---+
                                    |
                                    |  pipeline-error path
                                    +-------------------> [failed]
                                    |  (UPDATE: status, failure_stage,
                                    |   failure_reason, terminal_at)
                                    |
                                    |  watchdog path (after deadline)
                                    +-------------------> [failed, stage=crashed]
                                       (UPDATE same shape, fired by
                                        background scan, status guard
                                        prevents stomping a concurrent
                                        success)
```

Three properties of the machine:

- **Once terminal, always terminal.** `succeeded` and `failed` are absorbing states. The receipts table has no `UPDATE` site that transitions out of them.
- **Status guards every transition.** Every `UPDATE` includes `WHERE ... AND status='received'`. If two writers race (watchdog promoting to crashed while the pipeline is publishing success), the second write is a no-op and the loser logs a warning. SQLite serializes write transactions, so at most one of the two transitions actually succeeds.
- **Replay creates a new trace.** `sb borg replay <trace>` does not mutate the failed row's status; it creates a new receipts row with `replay_of=<original-trace>` and runs the pipeline against the same `raw_input`. The original failed row stays as the historical record of the prior attempt.

### Architecture

```
At the door (HTTP handler, Telegram bot, ntfy, discord, CLI):
  receipts::record_received(trace_id, method, raw_input)
    -> INSERT INTO receipts (trace_id, received_at, method, raw_input, status)
       VALUES (?, NOW(), ?, ?, 'received')
  (Synchronous. Before any pipeline work runs.)

Front-door rejection (auth, empty, malformed):
  receipts::mark_failed(trace_id, 'intake-rejected', reason)
    -> UPDATE receipts SET status='failed', failure_stage='intake-rejected',
       failure_reason=?, terminal_at=NOW() WHERE trace_id=?

Pipeline success:
  receipts::mark_succeeded(trace_id, note_path)
    -> UPDATE receipts SET status='succeeded', terminal_at=NOW(),
       note_path=? WHERE trace_id=?
  ledger::append_success(date, time, method, source, note_path, trace_id)
    -> append a row to borg-ledger.md

Pipeline failure (every error site in pipeline.rs):
  receipts::mark_failed(trace_id, <stage>, reason)
    -> UPDATE receipts SET status='failed', failure_stage=?,
       failure_reason=?, terminal_at=NOW() WHERE trace_id=?

Watchdog (every 60s):
  UPDATE receipts SET status='failed', failure_stage='crashed',
    failure_reason='no terminal event within ' || deadline_seconds || 's',
    terminal_at=NOW()
  WHERE status='received' AND received_at < (NOW() - deadline_seconds)
```

The watchdog stops being a markdown-table parser. The orphan-detection file (`borg-orphans.md`) stops existing. `borg-intake.md` stops existing.

### Data Model

#### `receipts` table (SQLite)

```sql
-- Mandatory PRAGMAs, applied on every connection open (rusqlite defaults
-- to DELETE journaling and a 0ms busy timeout; neither is acceptable for
-- concurrent daemon + CLI access).
PRAGMA journal_mode=WAL;
PRAGMA synchronous=NORMAL;
PRAGMA busy_timeout=5000;     -- ms; cover bursty contention without flapping
PRAGMA foreign_keys=ON;

CREATE TABLE receipts (
  trace_id        TEXT NOT NULL PRIMARY KEY,
  received_at     TEXT NOT NULL,                      -- ISO 8601 UTC
  method          TEXT NOT NULL,                      -- http | telegram | discord | ntfy | cli
  kind            TEXT NOT NULL                       -- url | text | binary
                    CHECK (kind IN ('url', 'text', 'binary')),
  raw_input       TEXT NOT NULL,                      -- url/text body for kind ∈ {url,text}; structured descriptor for kind='binary' (bytes live in sidecar)
  status          TEXT NOT NULL                       -- received | succeeded | failed
                    CHECK (status IN ('received', 'succeeded', 'failed')),
  terminal_at     TEXT,                               -- ISO 8601 UTC, set when status changes from 'received'
  note_path       TEXT,                               -- vault-relative path; set when status='succeeded'
  failure_stage   TEXT                                -- set when status='failed'
                    CHECK (failure_stage IN ('intake-rejected', 'classify-failed',
                                              'fetch-failed', 'quality-blocked',
                                              'pipeline-timed-out', 'publish-failed',
                                              'crashed')),
  failure_reason  TEXT,                               -- free-form, set when status='failed'
  replay_of       TEXT                                -- trace_id of the original failed attempt, if this row is a replay
);

CREATE INDEX idx_receipts_status ON receipts(status);
CREATE INDEX idx_receipts_received_at ON receipts(received_at);
CREATE INDEX idx_receipts_method_status ON receipts(method, status);

CREATE TABLE schema_version (
  version INTEGER NOT NULL PRIMARY KEY,
  applied_at TEXT NOT NULL
);
INSERT INTO schema_version (version, applied_at) VALUES (1, '2026-05-20T00:00:00Z');
```

`open_db` is responsible for issuing the PRAGMAs on every connection (WAL mode is per-database, not per-connection, so it survives across opens; `busy_timeout` is per-connection and must be re-issued). Unit tests for `borg::receipts::open_db` assert all four PRAGMAs are active post-open via `PRAGMA journal_mode;` etc. round-trips.

Storage path resolution: `dirs::data_local_dir()` joined with `sb/borg/receipts.db`. On Linux this is `~/.local/share/sb/borg/receipts.db`; on macOS it is `~/Library/Application Support/sb/borg/receipts.db`. No hardcoded `~/.local/share/` in user-facing strings.

**`raw_input` semantics by kind:** the `kind` column is the authoritative discriminator (URL / text / binary). For `kind='url'` and `kind='text'`, `raw_input` is the literal content. For `kind='binary'` (photos, audio, voice notes, documents from Telegram or ntfy attachments), `raw_input` is the structured descriptor string returned by `borg::intake::binary_descriptor` and the actual bytes continue to live at `system/intake/<trace_id>.txt` as a sidecar. The receipts DB therefore stores small UTF-8 values regardless of input size; binary blobs are not in the DB. `sb borg replay` consults `kind` to decide whether to re-inject `raw_input` directly (URL/text) or read bytes from the sidecar (binary) when reconstructing the pipeline payload.

**Concurrent-write ordering on the success path:** the pipeline performs ledger-append FIRST, then receipts UPDATE. Rationale: if the daemon crashes between the two writes, the next watchdog tick sees `status='received'` and promotes to `crashed` even though the ledger already has the success row. That's a benign drift (success in ledger, failed/crashed in receipts) detectable by a `sb borg audit` invariant check and reconcilable by a one-shot `sb borg audit --fix` UPDATE (`SET status='succeeded' WHERE ledger contains trace_id AND status='received-or-crashed'`). The opposite ordering (receipts first, then ledger) would yield a `succeeded` receipts row with no ledger row, which is harder to detect (no flag to look for). Drift in either direction is rare; the chosen ordering picks the direction that is cheap to audit.

#### `borg-ledger.md` schema

```markdown
| Date       | Time  | Method   | Source                        | Note                  | Trace      |
|------------|-------|----------|-------------------------------|-----------------------|------------|
| 2026-05-20 | 21:33 | telegram | https://thenewstack.io/...    | [[steve-yegge-s-...]] | tg-90db17  |
```

One row per successful ingestion. Strictly append-only at write time; no row is ever mutated. Reingest of an already-ingested source URL appends another row. Failures never appear. The `Status` column is removed entirely; every row is implicitly a success.

The "reingested or not" property that the dashboard surfaces is derived from the published note's frontmatter (`ingested:`, which is the date of the latest ingestion; older `ingested:` values can be tracked as a list if a future doc adds richer reingest history). It is not stored on the ledger row, because the ledger is now strictly append-only and storing a "was-replaced" flag would require mutating prior rows.

This is a deliberate departure from today's behavior: today, when a reingest succeeds, `vault::ledger::mark_replaced` flips the old row's status from `✅` to `🔄`. In the new model, both the original ingestion event and the reingest event have their own rows, both are successes, both stay in the ledger forever as historical facts. `mark_replaced` and the `LedgerStatus::Replaced` variant become dead code that Phase 4 removes; `LedgerStatus::Failed` likewise becomes dead code (failures live in receipts, not the ledger). The `LedgerStatus` enum collapses to a single `Completed` value or is removed entirely (the implementer can pick; the enum is no longer load-bearing).

The header frontmatter stays the same as today (`title: Borg Ledger`, type/domain/origin/tags). The "See also" line points only at `[[borg-dashboard]]` (no broken links to deleted files).

#### Removed: the four files that go away

`borg-intake.md`, `borg-dlq.md`, `borg-dlq-archive.md`, `borg-orphans.md`.

The migration (below) copies the relevant data into the receipts DB, then deletes the markdown files. The `vault/src/intake.rs`, `vault/src/dlq.rs`, and `borg/src/triage.rs` modules shrink correspondingly.

### API Design

#### CLI verbs

| Verb | Replaces | Behavior |
|------|----------|----------|
| `sb borg log [--status …] [--method …] [--since …] [--limit N] [--source PATTERN]` | `sb borg intake list`, `sb borg dlq list` | Queries the receipts DB. Default: most recent 20 rows, any status. |
| `sb borg show <trace>` | `sb borg intake show`, `sb borg dlq show` | One-trace detail: receipts row + sidecar reference + (if succeeded) note frontmatter snapshot. |
| `sb borg replay <trace>` | `sb borg dlq replay`, `sb borg reingest` | Re-injects the raw input from the receipts row's `raw_input` (or its sidecar for binary kinds) as a new trace with `replay_of=<original>`. |
| `sb borg audit [--fix]` | `sb borg audit` (today) | Same shape as today; now reads the receipts DB instead of `borg-intake.md`. Reports invariant breaches; `--fix` does targeted UPDATEs. |
| (removed) `sb borg dlq archive`, `sb borg dlq list --status pending`, `sb borg intake list` | -- | No more DLQ verb; failures are part of `sb borg log`. |

#### Library API

```rust
// borg/src/receipts.rs - new module
pub fn open_db(config: &Config) -> Result<Connection> { ... }
pub fn record_received(db: &Connection, trace_id: &str, method: Method, raw_input: &str) -> Result<()> { ... }
pub fn mark_succeeded(db: &Connection, trace_id: &str, note_path: &Path) -> Result<()> { ... }
pub fn mark_failed(db: &Connection, trace_id: &str, stage: FailureStage, reason: &str) -> Result<()> { ... }
pub fn promote_stale_to_crashed(db: &Connection, deadline_secs: u64) -> Result<usize> { ... }  // watchdog entry point
pub fn query(db: &Connection, filter: &Filter) -> Result<Vec<Receipt>> { ... }
pub fn get(db: &Connection, trace_id: &str) -> Result<Option<Receipt>> { ... }

// vault/src/receipts.rs - new module (replaces vault/src/dlq.rs;
// the DLQ concept goes away and the file's natural home is renamed).
pub enum FailureStage {
    IntakeRejected,
    ClassifyFailed,
    FetchFailed,
    QualityBlocked,
    PipelineTimedOut,
    PublishFailed,
    Crashed,  // renamed from DlqStage::WatchdogOrphan
}
impl FailureStage { pub fn as_str(&self) -> &'static str { ... } }
impl Display for FailureStage { ... }
impl FromStr for FailureStage { ... }
// The whole vault/src/dlq.rs file is deleted:
// DlqStage, DlqStatus, DlqEntry, parse_entries, append_entry,
// archive_resolved, update_status -> all gone.
// FailureStage lives in vault (not borg) because both borg's
// receipts module and the CLI display layer need to reference it
// across crate boundaries. Concrete SQLite code stays in borg/.

// vault/src/intake.rs - shrink to:
// IntakeKind, record_intake_with_sidecar (only the sidecar-writing half) stay.
// The markdown-table parsing/appending halves are deleted.
// intake_path() becomes obsolete and is removed.
```

#### Pipeline error-site wiring

Each error site in `borg/src/pipeline.rs` and downstream stages gets a corresponding `mark_failed` call with the right stage. Concretely (line numbers as of `cda6d54`):

| Error site | New stage |
|-----------|-----------|
| `pipeline.rs:790` `metadata_result.context("yt-dlp metadata failed")` | `FetchFailed` |
| `pipeline.rs:565` `eyre::bail!("Content quality check failed: ...")` | `QualityBlocked` |
| `pipeline.rs:679` `write_atomic` failure | `PublishFailed` |
| `pipeline.rs:1049` `eyre::bail!("fabric -u returned a block page ...")` | `FetchFailed` |
| `pipeline.rs:944` `yt-dlp video download timed out` | `FetchFailed` |
| `pipeline.rs:948` `eyre::bail!("yt-dlp failed: ...")` | `FetchFailed` |
| `pipeline.rs:327` outer `Err(_elapsed)` (hard timeout) | `PipelineTimedOut` |
| `pipeline.rs:320` outer `Ok(Err(e))` (catch-all) | `FetchFailed` (if no more specific stage was already recorded by an inner call) |
| classify stage failure | `ClassifyFailed` |
| `record_dlq` call sites in `routes.rs`, `telegram.rs`, `discord.rs`, `ntfy.rs` | `IntakeRejected` |
| watchdog promotion | `Crashed` |

The matching mechanism is a typed pipeline-error wrapper. A new type in `borg/src/pipeline/error.rs`:

```rust
#[derive(Debug)]
pub struct PipelineError {
    pub stage: FailureStage,
    pub source: eyre::Report,
}

impl PipelineError {
    pub fn new(stage: FailureStage, source: impl Into<eyre::Report>) -> Self {
        Self { stage, source: source.into() }
    }
}
```

`process_url_inner` and every fallible inner stage return `Result<T, PipelineError>` instead of `eyre::Result<T>`. Each error site constructs `PipelineError::new(FailureStage::FetchFailed, e)` (or whichever stage) at the point the error becomes terminal for the trace. `process_url`'s catch-all `Ok(Err(e))` arm now receives a typed `PipelineError`, calls `mark_terminal_failure(trace_id, e.stage, e.source.to_string())`, and the stage is type-checked at every error site rather than inferred from string matching. The implementer does not need to "remember" to classify; if a new error path is added without a stage, the code does not compile.

This is the only mechanism; there is no string-matching fallback and no `Cell<Option<FailureStage>>` thread-locally tracked thing. `PipelineError` is the contract.

### Implementation Plan

This is one back-to-back roadmap, not phased with soak time. Per `[[feedback-no-phase-gating]]`, all phases land in a single release cycle.

#### Phase 1: receipts module + schema + open/migrate
**Model:** sonnet
- Add `rusqlite` (with `bundled` feature), `r2d2`, and `r2d2_sqlite` deps to `borg/Cargo.toml` (`cargo add rusqlite --features bundled && cargo add r2d2 r2d2_sqlite`). No change to `cortex/Cargo.toml`; oracle keeps its existing rusqlite dep and gets no pool dep (its `failure_history` tool opens one read-only connection per MCP call).
- Create `vault/src/receipts.rs` with the `FailureStage` enum (lives in vault so borg + the sb CLI can both reference it). Delete `vault/src/dlq.rs` (DlqStage, DlqStatus, DlqEntry, parse_entries, append_entry, archive_resolved, update_status all go).
- Create `borg/src/receipts.rs` with: the schema above, `open_db` (creates the file and runs the schema migrations idempotently via `schema_version` table check, applies the four PRAGMAs to the opened connection), `build_pool` (builds an `r2d2::Pool<r2d2_sqlite::SqliteConnectionManager>` of size 8 with a `CustomizeConnection` impl that re-applies the four PRAGMAs to every fresh connection the pool creates), the `Receipt` struct, and the filter types. Every receipts-mutating function (`record_received`, `mark_succeeded`, `mark_failed`, `promote_stale_to_crashed`) takes a `&r2d2::Pool<...>` and internally runs the statement inside `tokio::task::spawn_blocking` after checking out a connection. CLI one-shot verbs call `open_db` directly.
- Create `borg/src/pipeline/error.rs` with the `PipelineError` wrapper type (see API Design).
- Add unit tests against a `:memory:` SQLite instance for `borg/src/receipts.rs`: open + apply schema + each of the record/mark/promote operations + query filter combinations + the status-guard race semantics.
- Wire `dirs::data_local_dir()` path resolution; never hardcode `~/.local/share/`.

#### Phase 2: wire receipts into the write paths
**Model:** opus
- At every front-door site (`routes.rs:83/151/371`, `telegram.rs:299/347/396/458/518`, `discord.rs:133/147`, `ntfy.rs:162`): call `receipts::record_received` synchronously before the existing pipeline dispatch.
- At every front-door rejection site: call `receipts::mark_failed(..., IntakeRejected, ...)` (replacing the current `intake_log::record_dlq` call).
- In `borg/src/pipeline.rs`: introduce `mark_terminal_failure(trace_id, stage, reason)` helper. Rewrite `make_failure` to call it; remove the `LedgerStatus::Failed` write to `borg-ledger.md`. Update each error-site listed in the table above to construct the correct stage. The catch-all branch uses `FetchFailed` only if no inner site already marked failure; preferred mechanism is a `Cell<Option<FailureStage>>` threaded through the call (or a typed wrapper around `eyre::Report`; implementer's choice).
- Update the success path (existing call to `ledger::append_entry` with `LedgerStatus::Completed`): add a `receipts::mark_succeeded` call alongside.
- Update `borg/src/watchdog.rs::run_once` to: (1) `SELECT trace_id, received_at FROM receipts WHERE status='received' AND received_at < (now - deadline_secs)`, (2) filter the result set in memory through `permits::is_trace_active` exactly as `watchdog.rs:99` does today, dropping any trace that is still queued for or holding a permit, (3) for each surviving trace, issue a targeted `UPDATE receipts SET status='failed', failure_stage='crashed', failure_reason='no terminal event within ' || deadline_secs || 's', terminal_at=NOW() WHERE trace_id=? AND status='received'` (the `AND status='received'` guard prevents a race against a concurrent success transition). The watchdog DOES NOT issue a bulk `UPDATE` without the permit filter; doing so would mass-promote queued bulk-upload traces to `crashed` and produce split-brain state when they later complete. Drop the markdown-table parsing entirely.

#### Phase 3: CLI verbs and reshape
**Model:** opus
- `sb borg log` (new), `sb borg show` (rewrite to read DB), `sb borg replay` (rewrite to read DB, consolidate the `dlq replay` + `reingest` surfaces).
- `sb borg audit` rewrite: invariant check is now (a) "every receipts row's `status` transitions properly within the deadline window" and (b) "no drift between ledger ✅ rows and receipts `succeeded` rows" (catches the crash-between-writes case described in Data Model). The `--fix` path runs `promote_stale_to_crashed` and reconciles drift via:
  ```sql
  -- Reconcile traces that have a ledger row but no terminal receipts row.
  -- Use parameterized binds (one row per trace_id) rather than a SQL-side
  -- join, since the ledger is a markdown file, not a table SQLite can read.
  UPDATE receipts
     SET status='succeeded',
         terminal_at=COALESCE(terminal_at, ?),   -- supplied: ledger row's timestamp
         note_path=COALESCE(note_path, ?)        -- supplied: ledger row's note link target
   WHERE trace_id=?                              -- supplied: ledger row's trace_id
     AND status IN ('received', 'failed')        -- failed means watchdog already promoted
     AND (status='received' OR failure_stage='crashed');  -- never overwrite a real failure
  ```
  The audit walks `borg-ledger.md`, collects the ✅ trace IDs that have no corresponding `succeeded` receipts row, and issues one parameterized UPDATE per drift entry. Empty drift set is the steady state.
- Remove `sb borg intake list/show`, `sb borg dlq list/show/archive/replay`. Add transitional `eprintln!` hints if anyone types the old verbs (one release of soft-removal text, then fully gone).
- Update `sb status` and `sb doctor` (`sb/src/cli/checks.rs`) to read receipts DB stats instead of parsing `borg-intake.md` / `borg-dlq.md`. The "dlq pending" health line becomes "receipts: N received, M succeeded, K failed (by stage: ...)".
- **Oracle integration:** add a new MCP tool `failure_history` in `oracle/src/server.rs` alongside the existing `ingest_history`. `failure_history` opens the receipts DB read-only at `dirs::data_local_dir()/sb/borg/receipts.db` (resolved via a new shared helper in vault::receipts), takes the same filter shape as `ingest_history` plus an optional `stage:` field, and returns the matching failed receipts. `ingest_history` itself is unchanged in behavior; it continues to read `borg-ledger.md` (now success-only after Phase 4). The two tools together give MCP consumers full visibility into both the success and failure histories. Oracle's Cargo.toml gets no new dep (it already has rusqlite). The receipts DB path resolution lives in `vault::receipts::receipts_db_path` so borg and oracle agree on the path without duplicating logic.

#### Phase 4: ledger reshape to success-only
**Model:** sonnet
- In `vault/src/ledger.rs`: delete `mark_replaced` (`vault/src/ledger.rs:269`) and its test (`test_mark_replaced_changes_status` at `vault/src/ledger.rs:628`). Delete `LedgerStatus::Replaced` and `LedgerStatus::Failed`; collapse the enum to a single `Completed` value, OR remove the enum entirely and have `LedgerEntry` carry no status field. Either is acceptable; the implementer picks.
- Update `LedgerEntry`'s on-disk row format to drop the `Status` column. Update `vault::ledger::append_entry` and the markdown rendering accordingly.
- Remove every non-test caller of `LedgerStatus::Failed` (the only writer is `make_failure` in `pipeline.rs`, removed in Phase 2).
- Rewrite the `borg-ledger.md` header text to remove references to failures and to remove the "See also: borg-dlq, borg-intake" line. The new header points only at `[[borg-dashboard]]`.

#### Phase 5: migration of existing data
**Model:** opus
- New verb `sb borg migrate-receipts` (one-shot, idempotent, safe to re-run; does NOT delete legacy files). The verb first checks `systemctl --user is-active borg.service`; if active, the verb refuses to run with a clear error ("borg.service must be stopped during migration; run `sb borg daemon --stop` first, then re-run `sb borg migrate-receipts`, then `sb borg daemon --start`"). This prevents racy reads/writes against the legacy markdown files during the migration pass.
  1. Open the receipts DB (creates if absent).
  2. Parse `borg-intake.md`. For each trace_id: `INSERT OR IGNORE INTO receipts (trace_id, received_at=row.date+row.time, method, raw_input=preview-or-sidecar, status='received')`.
  3. Parse `borg-ledger.md`. For each row: if status='✅' or '🔄', `UPDATE receipts SET status='succeeded', terminal_at=row.date+row.time, note_path=note-link-target WHERE trace_id=row.trace AND status='received'`. (Both ✅ and 🔄 map to `succeeded` in receipts; the 🔄 marker is information about a later mutation and the original ingestion event was a success.) If status='❌', `UPDATE receipts SET status='failed', failure_stage='unknown-legacy', failure_reason='migrated from pre-receipts ledger' WHERE trace_id=row.trace AND status='received'`. The `AND status='received'` guard makes the migration idempotent.
  4. Parse `borg-dlq.md` + `borg-dlq-archive.md`. For each row, depending on its DLQ stage:
     - `intake-reject` rows have no corresponding ledger row by construction (the input was rejected before the pipeline ran). For these: `UPDATE receipts SET status='failed', failure_stage='intake-rejected', failure_reason=row.reason WHERE trace_id=row.trace AND status='received'`. Without this case, IntakeReject traces would be stuck at `status='received'` after the migration and the next watchdog tick would mass-promote them to `crashed`, destroying their original taxonomy.
     - `watchdog-orphan` rows whose intake row had no ledger entry: `UPDATE receipts SET status='failed', failure_stage='crashed', failure_reason=row.reason WHERE trace_id=row.trace AND status='received'`.
     - All other DLQ stages (`fetch-failed`, `quality-blocked`, etc., for rows that DO have a corresponding `❌` ledger row): refine the already-failed receipts row: `UPDATE receipts SET failure_stage=row.stage, failure_reason=row.reason WHERE trace_id=row.trace AND status='failed' AND failure_stage='unknown-legacy'`.
  5. Rewrite `borg-ledger.md` atomically (write to `borg-ledger.md.tmp`, fsync, rename to `borg-ledger.md`): keep only the original ✅ and 🔄 rows; drop the Status column entirely; preserve original timestamps and trace IDs. Save with the new header text from Phase 4. The previous file is moved to `borg-ledger.md.pre-receipts` (so the operator has the original to compare).
  6. Print a verification summary the operator can sanity-check:
     ```
     Migrated <N> receipts:
       <X> succeeded (was: <X-prev> ✅ rows + <Y-prev> 🔄 rows = <X> in receipts)
       <K> failed   (was: <Z-prev> ❌ rows + <W-prev> watchdog-orphan rows = <K> in receipts)
                    failure_stage breakdown: <stage>: <count>, ...
       <R> still received (intake rows with no terminal record; will be promoted on next watchdog tick)
     Rewrote borg-ledger.md (<X> rows, was <X-prev>+<Y-prev>+<Z-prev> = <total-prev> rows).
     Saved original at borg-ledger.md.pre-receipts.
     ```
     Expected concrete numbers given today's vault: X=1260 (1124 ✅ + 136 🔄), K=50 + however-many DLQ rows that don't have a corresponding ledger row, R should be 0 after the next watchdog tick.
- Separate verb `sb borg migrate-receipts --prune-legacy` (second step, run only after the user has verified the receipts DB and the rewritten ledger look correct):
  1. **User-vault backlink scan:** grep the entire vault (excluding `system/views/`) for `[[borg-intake]]`, `[[borg-dlq]]`, `[[borg-dlq-archive]]`, `[[borg-orphans]]` references. If any are found, print them with file paths and refuse to prune until the operator either removes them, replaces them with `[[borg-ledger]]` / "see `sb borg log --status failed`", or re-runs with `--prune-legacy --force`. Rationale: a user can wiki-link to the DLQ page from a personal note (a retro on a failed ingestion, a TODO referencing a trace, etc.); silently deleting the linked page breaks their Obsidian graph.
  2. After the scan passes (no broken backlinks would result), `rkvr rmrf` the four old markdown files (`borg-intake.md`, `borg-dlq.md`, `borg-dlq-archive.md`, `borg-orphans.md`) and the `.pre-receipts` backup of the ledger. The user runs this verb explicitly; bootstrap does not auto-prune.
- `sb bootstrap` detects the presence of the four old files and prints a one-line nudge: "Run `sb borg migrate-receipts` to consolidate legacy bookkeeping into the receipts log; then `sb borg migrate-receipts --prune-legacy` once you've verified."

#### Phase 6: docs + CLAUDE.md amendment
**Model:** sonnet
- Update `CLAUDE.md`:
  - Architecture section: change "Borg writes only to the vault filesystem (markdown files + staged artifacts). Oracle owns the SQLite FTS5 index" to "Borg writes the vault filesystem (markdown files + staged artifacts) AND its own SQLite receipts log at `~/.local/share/sb/borg/receipts.db`. Oracle owns its own separate SQLite FTS5+vector index. The two SQLite files are different files with different writers; nothing in borg opens oracle's DB and nothing in oracle opens borg's."
  - Remove the "every trace_id must appear in borg-ledger.md (success path) or borg-dlq.md (failure path)" sentence; replace with "Every input borg receives is durably recorded in `~/.local/share/sb/borg/receipts.db` with status=received at intake time, then mutated in place to succeeded or failed at terminal time. The success subset is also appended to `borg-ledger.md` for in-Obsidian browsing."
  - Update the "Borg durable-capture stores" section to describe the receipts log + sidecar pair instead of the intake.md + dlq.md pair.
- Update `borg-ledger.md` frontmatter description in the migration step (Phase 5).
- Update `borg/src/dashboard.rs`'s embedded template string (`dashboard.rs:81-87`): remove the "## ⚠️ Recently failed (DLQ)" and "## 🕳️ Intake without resolution (orphans)" sections and their `[[borg-dlq]]`/`[[borg-orphans]]` wikilinks. (The currently rendered `borg-dashboard.md` does not contain these sections, but the template still does; the template is the source of truth and is what `sb borg dashboard --refresh` would write. Cleaning the template prevents the next refresh from regressing the dashboard with broken wikilinks.)
- If a user has manually re-added those sections to `borg-dashboard.md`, the migration verb (`sb borg migrate-receipts`) detects `[[borg-dlq]]` or `[[borg-orphans]]` references in the rendered dashboard and warns: "borg-dashboard.md still references deleted pages; edit manually or run `sb borg dashboard --refresh`."

### Operator workflow: before and after

| Task | Before | After |
|------|--------|-------|
| "What's new in my vault?" | Open `borg-dashboard.md`, browse the Dataview sections (today, week, etc.). | Identical. The dashboard is unchanged. |
| "Did I ingest this URL?" | `grep <url> borg-ledger.md` | `grep <url> borg-ledger.md` (the file is still there, success-only). Or `sb borg log --source <url-pattern>` to also see failed attempts. |
| "Why did this URL fail?" | Open `borg-dlq.md` in Obsidian, search for the trace; reason is usually "watchdog-orphan, no ledger or dlq row within 1860s" (i.e., generic). Then dig in the borg log file for the real error. | `sb borg show <trace>` shows status, failure_stage, failure_reason, raw_input. The real error is right there. |
| "List my recent failures." | Open `borg-dlq.md`, eyeball. | `sb borg log --status failed --since "24h ago"` |
| "Replay this failed trace." | `sb borg dlq replay <trace>` (existing) | `sb borg replay <trace>` (consolidated verb; reads raw_input from the receipts DB). |
| "How many failures by stage?" | Not really queryable; everything's `watchdog-orphan`. | `sb borg log --status failed --group-by stage` |
| "Triage the DLQ." | Open `borg-dlq.md`, mark rows resolved one by one, run `archive --resolved` to sweep. | Failures stay in the receipts DB indefinitely. No archival concept; the operator just doesn't look at them unless investigating. The receipts DB is not in the operator's Obsidian view. |

## Alternatives Considered

### Alternative 1: JSONL append-only event log
- **Description:** Replace the receipts DB with a single JSONL file at `~/.local/share/sb/borg/receipts.jsonl`. Each event (received, succeeded, failed) is one line. Current state of a trace = last line for that trace_id.
- **Pros:** No new dependency. `jq` and `grep` work. Append-only is the natural durability shape. Easy backup.
- **Cons:** Every read scans the whole file and folds by trace_id. At today's throughput (hundreds of receipts/month) this is fine; at multi-year scale (tens of thousands) it gets sluggish. No native "show me one row" - every read scans. Concurrency is tricky: append is safe under POSIX (atomic up to PIPE_BUF), but the same write-lock dance the markdown tables do today would still be needed for safety on long records. Schema evolution is by convention only; no enforced types.
- **Why not chosen:** SQLite gives us status-update-in-place as a single UPDATE (one disk round-trip), proper indexes on `status`/`received_at`/`method`, and schema enforcement via `CHECK` constraints. The dependency cost is small (`rusqlite` is widely deployed, already in oracle); the architecture-rule amendment is the real cost, and it is small and clean to make.

### Alternative 2: Per-trace JSON sidecars
- **Description:** One JSON file per trace at `~/.local/share/sb/borg/receipts/<trace_id>.json`. Mutated in place at status transitions. Queries walk the directory.
- **Pros:** No deps. Per-trace atomic writes (single file = atomic by rename). Crash-safe per trace. Builds on the existing `system/intake/<trace>.txt` sidecar pattern.
- **Cons:** "List all failures since yesterday" walks the whole directory. Inode pressure if it grows unbounded (tens of thousands of small files). Filesystem isn't a query engine; everything is a directory scan.
- **Why not chosen:** Same shape as JSONL but worse: trades JSONL's "one file to back up" for "one directory of tens of thousands of files." Solves a problem we don't have (per-trace atomicity is already covered by SQLite's WAL mode).

### Alternative 3: Keep markdown, fix the wiring
- **Description:** Keep all six markdown files; fix only the unwired `DlqStage` variants in the pipeline error path; do not introduce SQLite.
- **Pros:** No new dependency. No CLAUDE.md amendment. Smaller change.
- **Cons:** Leaves all the accretion problems: failures still in the operator's Obsidian view, intake.md still a daemon-internal file exposed to the user, archive-watchdog death spiral still latent, dashboard-vs-ledger scope unchanged, markdown table parsing still the system of record. Fixes one of seven concerns; calls victory while six are unaddressed.
- **Why not chosen:** Per `[[feedback-no-deferments]]`, "everything" means full coverage. A partial fix here leaves the reshape unfinished and forces a second pass later.

### Alternative 4: Collapse to one markdown file (just `borg-dashboard.md`)
- **Description:** Drop `borg-ledger.md` entirely. Add a Dataview block to the dashboard for "all ingestions, chronological," sourced from the receipts DB via a hypothetical Dataview SQLite bridge.
- **Pros:** One Obsidian-facing file. Conceptually clean.
- **Cons:** Obsidian Dataview cannot query SQLite directly; the dataview block would need to read from the published notes, which already excludes failures. The user explicitly noted that Dataview performance degrades at high counts; a 1124-row Dataview block would be the slowest section in the vault. Also drops the "grep-friendly raw URL chronology" use case which is exactly why `borg-ledger.md` exists as a static markdown table in the first place.
- **Why not chosen:** Discussed with the user; the conclusion was to keep both `borg-ledger.md` (static markdown chronology, grep-friendly, no Dataview cost) and `borg-dashboard.md` (Dataview, browseable, windowed) with the scope split documented in the Architecture section.

### Alternative 5: True DLQ with retries
- **Description:** Implement retry semantics on transient failures (network blips, 503s, timeouts). Wire up `DlqStatus::Retried` and a per-row `retries` counter. A failure is "dead" only after N retries are exhausted; the receipts log carries a `retry_count` column and an `is_dead` projection.
- **Pros:** Matches the customary industry "DLQ = retries-exhausted dead-letter" pattern. Recovers more inputs automatically.
- **Cons:** Requires classifying every failure as retriable vs permanent (significant new policy surface). Increases the number of fabric / yt-dlp / network calls per input. Increases the surface for "wedged retry loop" failures. The user explicitly said no retry semantics in the goals-setting conversation.
- **Why not chosen:** Explicitly out of scope per user direction; covered in "Non-Goals."

## Technical Considerations

### Dependencies

- **New deps in `borg/Cargo.toml`:**
  - `rusqlite` with the `bundled` feature (`cargo add rusqlite --features bundled`). The bundled feature avoids depending on the host's libsqlite3 and makes the binary self-contained, matching `[[feedback-self-contained]]`.
  - `r2d2` and `r2d2_sqlite` (`cargo add r2d2 r2d2_sqlite`). Provides the connection pool that the daemon uses (see Connection Model below). The pool's connection customizer is the single place where every PRAGMA from the schema block is applied to a fresh connection.
- **No new dep in `cortex/Cargo.toml`** (cortex does not write the receipts log).
- **No new dep in `oracle/Cargo.toml`** (oracle already has rusqlite for its FTS5 index; that dep stays; the two SQLite files are different files). Oracle opens the receipts DB read-only for the new `failure_history` MCP tool; one connection per MCP call, no pool needed on oracle's side.
- **No new dep in `vault/Cargo.toml`** (the FailureStage enum is plain types; no SQLite code lives in vault).

### Connection model

The daemon (`sb borg daemon --start`) and the CLI (`sb borg log/show/replay/audit`) have different concurrency profiles and use different connection patterns.

**Daemon: small `r2d2` connection pool.** The daemon creates a pool of size 8 at startup. Every receipts-touching code path (`record_received`, `mark_succeeded`, `mark_failed`, `promote_stale_to_crashed`, etc.) checks out a connection from the pool, runs its statement inside `tokio::task::spawn_blocking`, and returns the connection on drop. Rationale (per Architect round 3): `rusqlite` is a synchronous, blocking library. Wrapping a single shared connection in a `tokio::sync::Mutex` would stall a Tokio worker thread on every fsync; wrapping it in a `std::sync::Mutex` would risk async executor starvation. A pool plus `spawn_blocking` lets concurrent pipelines (bounded by `HEAVY_PERMITS`) each operate without contending on a Rust-level lock; SQLite's WAL mode and `busy_timeout=5000` serialize concurrent writes at the C level without flapping. Pool size of 8 covers the worst-case concurrent set: up to `HEAVY_PERMITS` pipelines + 1 watchdog + 1 front-door receiver + headroom.

**CLI (one-shot verbs): open + close per invocation.** `sb borg log` and friends do not need a pool; they're a single statement per process invocation. Each verb opens one connection via the same `borg::receipts::open_db` helper the pool uses (so PRAGMAs are applied identically), runs its query, and exits. No coordination with the daemon's pool is needed; SQLite WAL handles the cross-process concurrency.

**Connection PRAGMA enforcement.** The pool's connection customizer (`r2d2::CustomizeConnection`) issues the four mandatory PRAGMAs (`journal_mode=WAL`, `synchronous=NORMAL`, `busy_timeout=5000`, `foreign_keys=ON`) on every newly-created connection. The one-shot CLI path uses the same helper. There is no second code path that creates connections without PRAGMAs.

### Retention

The receipts DB is append-and-update-forever. No automatic pruning. Rationale: at the user's ingest rate (low hundreds per month), a decade of usage is ~100K rows; indexed SQLite handles that in a few MB and single-digit-ms queries. Auto-pruning introduces background-job complexity, risks deleting data the user might want to inspect ("when did I last try this URL?"), and solves a disk-space problem we do not have. If a future operator hits a scale where the table is unwieldy, a follow-up doc adds a `sb borg log --prune-before YYYY-MM-DD` verb; that verb is **not** in this design and is **not** implicitly future-required. The receipts DB is the durable audit trail and lives forever by default.

### Performance

- **Write latency at the door.** A single `INSERT INTO receipts ...` in SQLite WAL mode is ~10-50µs on local SSD. Synchronous, well under any user-perceptible latency on Telegram/HTTP receipt.
- **Read latency for `sb borg log --status failed --since today`.** With the `idx_receipts_status` and `idx_receipts_received_at` indexes, ~1ms even at 100K rows. Today's markdown-table parsing of `borg-ledger.md` (208K, 1300+ rows) on every `sb borg dlq list` takes ~50ms.
- **Watchdog tick.** One `UPDATE` with a `WHERE` clause covered by `idx_receipts_status` (`status='received'`) and a range scan on `received_at`. ~1ms at any realistic row count. Replaces a full-file parse of `borg-intake.md` + a full-file parse of `borg-dlq.md` + an append.
- **`borg-ledger.md` growth.** Today: ~150 bytes/row, ~1300 rows = 208K. Static markdown table renders in source mode trivially at any size. Preview-mode (Obsidian's rendered view) starts to lag at multi-thousand-row tables but stays usable to 10K+. Beyond that, we add a "ledger annual rollover" verb in a future doc.

### Security

- **Receipts DB contains raw URLs the user sent.** Same sensitivity class as `borg-intake.md` today (the URLs are already on disk). No new secret material. Path permissions: SQLite respects umask; the directory is `~/.local/share/sb/borg/` which is mode 0700 on a standard XDG layout. We do not write secrets (tokens, API keys) to receipts; raw_input is whatever the user sent, which on `http`/`telegram`/`ntfy` is a URL or short text.
- **No new network surface.** The receipts DB is a local file; no listener.
- **Concurrent writers.** The daemon's pipeline workers use the pool described in Connection Model; SQLite WAL serializes their writes at the C level via `busy_timeout=5000`. CLI verbs that read (`sb borg log/show/audit`) open a read-only connection per invocation; reads run concurrently with daemon writes (WAL permits this). CLI verbs that write (`sb borg replay`, `sb borg migrate-receipts`) acquire a write lock through the same SQLite serialization; `sb borg migrate-receipts` additionally refuses to run if `borg.service` is active (Phase 5 safety check). No exclusive-lock or daemon-channel mechanism is needed.

### Testing Strategy

- **Unit tests against `:memory:` SQLite.** Every function in `borg/src/receipts.rs`: open + apply schema, each record/mark/promote/query path, edge cases (mark_succeeded on a not-yet-received trace, mark_failed twice with different stages, status check constraint violation, etc.).
- **Watchdog tests.** Promote-stale logic with synthetic `received_at` values across the deadline boundary; verify only the past-deadline rows transition.
- **Migration tests.** Build a tempdir with synthetic `borg-intake.md`, `borg-ledger.md`, `borg-dlq.md`, `borg-dlq-archive.md` and verify the migration produces the right receipts DB. Edge cases: a trace in intake but not in ledger or dlq (becomes `received` then promoted to `crashed` on the next watchdog tick); a trace in ledger ✅ but not in intake (impossible by today's invariant, but assert clean behavior); a 🔄 ledger row (stays succeeded in receipts; the replacement note's trace gets its own receipts row).
- **Integration tests.** A small mini-vault fixture, run a full pipeline that fails at fetch-stage, verify (a) the trace's receipts row went `received -> failed` with `failure_stage='fetch-failed'`, (b) no ledger row was written, (c) `sb borg log --status failed` finds it, (d) `sb borg replay <trace>` creates a new trace with `replay_of=<original>`.
- **Existing tests pass.** Every test in `borg/src/`, `cortex/src/`, `vault/src/`, `oracle/src/` continues to pass after the change. The `vault::table` markdown-table tests stay (other callers still use them); the `vault::intake::parse_entries` and `vault::dlq::parse_entries` tests get deleted along with the code they exercise.

### Rollout Plan

Per `[[feedback-no-phase-gating]]`, no soak time between phases. All six phases land in one release. The rollout uses **dual-write** to give the operator a verification window before the new store is the only store.

Dual-write intent: during Phases 2-5, every write path writes to BOTH the receipts DB AND the legacy markdown table it replaces (e.g., intake.md AND `receipts.db status='received'`). The watchdog still parses `borg-intake.md` and writes `borg-dlq.md` AS WELL AS calling `promote_stale_to_crashed`. CLI reads switch over to the receipts DB; CLI verbs that target legacy files print a deprecation hint. If anything goes wrong, the legacy markdown files have the same content they would have had without this change, so rollback is "revert the bump tag and reinstall the prior release."

Sequence in a single PR or a small stack:

1. **Land Phase 1-2** first (receipts module + write paths). New code is dual-wired: writes go to BOTH the receipts DB and the markdown tables.
2. **Land Phase 3-4** (CLI verbs read from DB; ledger trimmed to success-only). Reads switch to receipts DB. Dual-writing continues.
3. **Land Phase 5** (migration command). The verb is available but not run by `otto deploy`. Old markdown files exist on disk and still get appended-to.
4. **Land Phase 6** (docs + CLAUDE.md amendment, dashboard template cleanup).
5. Operator runs `sb borg migrate-receipts` once. Verifies receipts DB matches expectations (`sb borg log --status succeeded | wc -l` matches old `borg-ledger.md` row count, etc.).
6. After verification, operator runs `sb borg migrate-receipts --prune-legacy`. The four old markdown files are deleted (via `rkvr rmrf`, so recoverable). A follow-up commit removes the dual-write code paths (they become dead code referencing files that no longer exist). Ship that as the next patch.

The dual-write window is bounded by the operator's verification step, not by calendar time. `otto deploy` does not require a flag day; the upgrade is `bump -m` + `otto deploy` + one explicit `sb borg migrate-receipts` + (after verification) one explicit `sb borg migrate-receipts --prune-legacy`.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| SQLite file corruption (power loss, FS bug) | Low | High | WAL mode + `synchronous=NORMAL`; the receipts log is reconstructable from `borg-ledger.md` + the per-trace sidecars (raw inputs). A `sb borg rebuild-receipts` verb can be added later (out of scope here) that walks the sidecars and the ledger and reconstructs the DB. |
| Migration corrupts the legacy markdown before the DB is verified | Low | High | `sb borg migrate-receipts` does NOT delete the old markdown files in the same run. Two-step: migrate (creates DB, leaves markdown alone), then a second invocation with `--prune-legacy` deletes them. The user verifies between steps. `rkvr rmrf` archives the deleted files so recovery is one command. |
| Architecture-rule amendment ("borg may depend on rusqlite") triggers downstream surprise (e.g., some test that asserts the dep graph) | Low | Low | The amendment is a sentence in CLAUDE.md; no automated test asserts the dep graph. Phase 6 includes the amendment commit. |
| The watchdog's UPDATE races against an in-flight pipeline completion | Low | Medium | Both operations take a SQLite write lock; SQLite serializes them. The pipeline completes by issuing `UPDATE receipts SET status='succeeded' WHERE trace_id=? AND status='received'` (note the status guard); if the watchdog already promoted to `failed`/`crashed`, the success UPDATE is a no-op and the pipeline logs a warning ("received->succeeded race lost, trace was already marked crashed"). Same hazard exists in the current intake/ledger model and is handled the same way: by status guards. |
| Failure-stage classification drift: a new error path is added in pipeline.rs but the author forgets to call `mark_terminal_failure` | Medium | Low | Add a `#[must_use]` discipline: `mark_terminal_failure` is the only path to publish a typed failure stage; the catch-all branch in `process_url` calls it with `Crashed` if no inner stage was recorded (the `Cell<Option<FailureStage>>` mechanism described in Phase 2 makes this enforceable at the call-site). |
| Dashboard's reference to deleted `[[borg-dlq]]` page leaves a dead wikilink | Low | Low | Phase 6 includes editing the dashboard's "DLQ failures" panel; the dead-link risk is zero if Phase 6 lands in the same release. |
| User loses receipts DB and didn't back it up | Low | Medium | Same risk as today's `borg-intake.md`/`borg-ledger.md` files; whatever vault-backup strategy the user runs (git, syncthing, restic) needs to also back up `~/.local/share/sb/borg/`. Documented in CLAUDE.md amendment. |
| Migration runs while borg.service is active and races against a live ingest | Low | High | Phase 5 verb refuses to run if `borg.service` is active. The operator must stop it explicitly, run the migration, then start it. The check is `systemctl --user is-active borg.service`. |
| Oracle's `ingest_history` MCP tool returns stale-looking output (no failures) after migration | Medium | Low | Phase 3 adds `failure_history` as a sibling tool. Document the split in the tool's `description` text so MCP clients (and the LLM consumer) understand `ingest_history` is success-only and `failure_history` is for failures. |
| Cortex test fixture references `borg-ledger.md` with the old schema (`cortex/src/links.rs:219`, `cortex/src/testutil.rs:158`) | Low | Low | These are test fixtures only; the production code path does not read the ledger. Phase 4 includes updating the fixture strings to the new no-Status-column format, so tests don't drift. |
| Watchdog mass-promotes queued bulk-upload traces to `crashed` | Medium | High | The watchdog filters its `SELECT` result set through `permits::is_trace_active` in memory before issuing UPDATEs; any trace currently queued for or holding a permit is excluded even if its intake age has crossed the deadline. This is the same load-bearing guard `watchdog.rs:99` enforces today and is preserved in Phase 2's spec. |
| `IntakeReject` traces lose their failure_stage during migration | Medium | Medium | Phase 5 Step 4 has an explicit branch for `intake-reject` DLQ rows that transitions `received` → `failed` with `failure_stage='intake-rejected'`. Without this branch they would sit at `received` until the next watchdog tick mass-promoted them to `crashed`. |
| `rusqlite` defaults to DELETE journaling, causing `database is locked` errors under concurrent daemon + CLI access | Medium | Medium | `borg::receipts::open_db` issues `PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=5000; PRAGMA foreign_keys=ON;` on every connection open. Unit tests assert all four are active post-open. |
| Wiki-links from user notes to `[[borg-dlq]]` etc. break when prune-legacy deletes the pages | Medium | Low | `sb borg migrate-receipts --prune-legacy` greps the vault (excluding `system/views/`) for references to the four pages before deleting; refuses to proceed if any are found unless `--force` is passed. |

## References

- `~/repos/scottidler/second-brain/CLAUDE.md` (project context)
- `~/repos/scottidler/second-brain/docs/design/2026-04-19-staged-ingestion-pipeline.md` (the staged pipeline + sidecar design that this builds on)
- `~/repos/scottidler/second-brain/docs/design/2026-05-08-borg-pipeline-resilience.md` (resilience patterns)
- `~/repos/scottidler/second-brain/vault/src/dlq.rs` (the current DlqStage enum)
- `~/repos/scottidler/second-brain/borg/src/pipeline.rs` (the error paths to wire)
- `~/repos/scottidler/second-brain/borg/src/watchdog.rs` (the watchdog that becomes a single SQL UPDATE)
- Memory: `[[feedback-design-doc-first]]`, `[[feedback-no-deferments]]`, `[[feedback-no-phase-gating]]`, `[[feedback-self-contained]]`, `[[feedback-no-unbounded-fanout]]`
