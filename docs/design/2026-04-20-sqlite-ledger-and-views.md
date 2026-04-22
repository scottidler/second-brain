# Design Document: SQLite Ledger + Snapshot Views

**Author:** Scott Idler
**Date:** 2026-04-20
**Status:** Draft
**Review Passes Completed:** 5/5 + architect review

## Summary

Move the borg ledger from a monolithic 664KB markdown table to an indexed SQLite store, and regenerate `system/views/borg-dashboard.md` and `system/views/borg-ledger.md` as pre-rendered markdown snapshots on a short tick plus post-ingest hook. The files stay ordinary vault notes — Obsidian Sync carries them to mobile exactly as today, Dataview disappears from these views, and all dashboard queries become indexed SQL against the ledger. No Obsidian plugin dependencies, no localhost HTTP surface, no mobile-desktop split. The SQLite store is the new truth; markdown is a pure view that borg maintains.

## Problem Statement

### Background

`system/views/borg-ledger.md` is a single markdown table that borg appends to on every ingest. It has grown to 664KB / 1042 rows in roughly three months and grows linearly with ingestion volume.

`system/views/borg-dashboard.md` contains five Dataview queries (Added Today, Yesterday, This Week, This Month, Stats). Each query scans every note in the vault on every page open, because Dataview indexes frontmatter across the entire vault.

Borg already runs as a systemd user daemon with an axum HTTP server (`borg/src/routes.rs`) for ingest endpoints. It already writes dashboard and ledger markdown files (`borg/src/dashboard.rs`, `borg/src/ledger.rs`). Cortex runs as a separate systemd user daemon with vault-scanning machinery for hygiene, linking, and quality reports. Oracle is an MCP server that reads the vault on demand.

### Problem

1. **Dataview scales poorly.** Each dashboard query is O(vault-size) and runs on every note open. With a few thousand notes the dashboard visibly stalls; at current ingest rate this worsens linearly.
2. **The ledger markdown is a single 664KB table.** Obsidian re-indexes it on every vault open (full-text search), and rendering the full table in preview mode is expensive. Git diffs in the vault are dominated by ledger-line appends.
3. **Writes contend on a file lock.** `vault::ledger::append_ledger_entry` takes an fs2 advisory lock on a growing file. Lock duration increases with file size.
4. **No query surface.** "Ingests from xda-developers this week" is effectively impossible in markdown. Oracle cannot answer ledger-shaped questions. Retention is manual.
5. **Truth and presentation are fused.** The ledger markdown table is simultaneously the canonical record of ingestion events and the human-readable view of them. Improvements to either form require touching both.

### Goals

- Move ledger truth to an indexed store with O(log n) queries.
- Separate truth from presentation. The markdown files in the vault become views of the store, not the store itself.
- Preserve in-Obsidian dashboard and recent-ledger UX. The user opens the same two notes and sees the same information, faster and fresher.
- Keep mobile working identically to desktop. Mobile sees the snapshot markdown via Obsidian Sync; there is no desktop-only rendering path.
- Fit cleanly into the existing borg daemon (WAL-safe SQLite colocated with borg's stages directory).
- Enable a richer query surface for the CLI, oracle MCP, and any future consumer.
- Ship with zero new Obsidian plugin dependencies.

### Non-Goals

- Replacing Obsidian as the primary reading environment.
- A localhost HTTP / APIRequest-plugin rendering path. Explicitly considered and rejected (see Alternative 6) because it would be desktop-only and therefore useless for the mobile use case that Obsidian Sync already covers via the snapshot. If a desktop-only live panel ever becomes interesting (TUI, custom plugin, etc.), it can be added on top of the SQLite store in a future design without disturbing this one.
- Moving notes themselves into SQLite. Notes remain markdown files in the vault.
- Multi-writer concurrency. Only borg writes the ledger table. Cortex and oracle are read-only consumers.
- Rewriting cortex, oracle, or the ingestion pipeline. They become additional readers of the new store.
- Historical archive of the monolithic `borg-ledger.md`. After validated migration it is replaced by the regenerated snapshot.

## Proposed Solution

### Overview

Two layers, shipped in order:

1. **Storage (Phase 1-3).** SQLite at `~/.local/share/borg/ledger.db`, written by the existing ingest pipeline, with a dual-write period where both SQLite and the current markdown ledger receive rows. Old markdown parsed and imported via a one-shot migration subcommand.

2. **Snapshot renderer (Phase 4).** A borg subcommand and periodic task regenerates `system/views/borg-dashboard.md` and `system/views/borg-ledger.md` as pure pre-rendered markdown (no Dataview, no code blocks). The files stay ordinary vault notes, so Obsidian Sync carries them to mobile unchanged. Freshness comes from a post-ingest hook (renders immediately when new rows land) plus a short safety tick.

### Architecture

```
┌──────────────────────────────────────────────────────────┐
│ borg daemon (systemd user service)                       │
│                                                          │
│  ingest pipeline ─► vault::db::ledger::insert ─┐         │
│                                                ▼         │
│                               ~/.local/share/borg/       │
│                                     ledger.db            │
│                                     (WAL mode)           │
│                                       │                  │
│                                       ▼                  │
│           snapshot renderer (post-ingest + 300s tick)    │
│           ─ queries ledger.db                            │
│           ─ writes system/views/borg-dashboard.md        │
│           ─ writes system/views/borg-ledger.md           │
│           ─ atomic rename, skip-if-identical             │
└──────────────────────────────────────────────────────────┘
                           │
                           │ Obsidian Sync (paid)
                           ▼
                    desktop + phone see same markdown
```

Additional readers: `borg ledger` CLI, oracle MCP tools, cortex quality reports — each opens its own read-only SQLite connection against the same DB file (WAL mode makes multi-process reads safe).

### Data Model

```sql
-- Schema version 1
CREATE TABLE schema_version (
  version    INTEGER PRIMARY KEY,
  applied_at INTEGER NOT NULL
);

CREATE TABLE ledger (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  trace_id    TEXT    NOT NULL UNIQUE,
  method      TEXT    NOT NULL,   -- 'cli' | 'telegram' | 'http' | 'discord' | 'ntfy' | 'clipboard'
  status      TEXT    NOT NULL,   -- 'completed' | 'failed' | 'skipped' | 'replaced'
  source_url  TEXT    NOT NULL,   -- canonical URL (utm_*, fbclid, gclid, fragments already stripped upstream by borg::hygiene::canonicalize_url)
  domain      TEXT,               -- host extracted from source_url; null for non-URL captures
  title       TEXT,               -- null for failed/skipped
  filename    TEXT,               -- relative vault path hint; see filename-stability note below
  reason      TEXT,               -- null for completed; populated for failed/skipped
  supersedes  TEXT,               -- trace_id of the prior attempt this row replaces (retry/refresh); NULL for fresh captures
  created_at  INTEGER NOT NULL,   -- unix seconds, UTC, when the ledger row was inserted
  ingested_at INTEGER             -- unix seconds, UTC, when the vault note was physically written; populated only when status='completed', NULL for failed/skipped/replaced
);

CREATE INDEX idx_ledger_created_at ON ledger(created_at);
CREATE INDEX idx_ledger_domain     ON ledger(domain);
CREATE INDEX idx_ledger_status     ON ledger(status);
CREATE INDEX idx_ledger_method     ON ledger(method);
CREATE INDEX idx_ledger_source_url ON ledger(source_url);  -- powers the supersedes lookup on ingest
```

Notes on the schema:

- `trace_id` is the identifier borg already produces (`cl-40d74f`, `tg-0f182e`, etc.) and is the stable identity of each ledger event. `UNIQUE` + `INSERT OR IGNORE` gives **crash-safe append**: if a process crashes between the ingest and the ledger insert and is rerun with the same trace_id, the second attempt is a no-op. The pipeline generates distinct trace_ids per capture event, so user-initiated resubmits of the same URL are legitimately different trace_ids — see `supersedes` below for how they get linked.
- `source_url` stores the **canonical URL** produced by `borg::hygiene::canonicalize_url` (the same function the existing `vault::ledger::check_duplicate` already keys on). Tracking params, fragments, and redirect noise are stripped upstream before they reach the ledger. No schema-level canonicalization is needed.
- `supersedes` implements the retry/refresh marker. On ingest, borg runs a SELECT against `idx_ledger_source_url` for the most recent row with matching canonical URL within a configurable window (default 7 days, `ledger.retry-window-days`). If a hit is found, the new row's `supersedes` is set to that prior row's trace_id. Both rows remain; the retry points back. Renderers display `♻️` on rows where `supersedes IS NOT NULL`. This preserves the event-log property (every attempt is recorded) while giving the dashboard a clear visual signal for "this is a retry / refresh, not a fresh capture."
- **Filename stability.** `filename` is a convenience hint column, not a join key. If the user renames a note in Obsidian, the vault path drifts and this column goes stale — borg has no rename watcher. `trace_id` is the stable identity: every note emitted by borg carries `trace: <trace_id>` in its frontmatter (per the staged-pipeline design at `docs/design/2026-04-19-staged-ingestion-pipeline.md`), so renderers that need a current vault link resolve `trace_id → vault file` at render time via a frontmatter lookup (cached by cortex if hot). A future cortex rename-watcher may opportunistically update `filename`; until then, treat the column as a hint, not ground truth.
- `created_at` is a unix second integer in UTC, not an ISO string. Range queries stay cheap. **All timezone handling lives in Rust, not SQL.** "Today" / "yesterday" / "this week" bounds are computed in the user's local timezone using `chrono::Local` (which respects the system TZ database, so PST becomes PDT across DST automatically) and passed as parameterized `>= ? AND < ?` predicates. SQL never calls `strftime('now', ...)` — that would evaluate in UTC and silently return the wrong day near midnight local. Renderers likewise convert to local time at display via `chrono::Local`.
- `status` is stored as lowercase string rather than integer for readable SQL and easy joins. The existing `LedgerStatus` enum in `vault/src/ledger.rs` gains `to_sql_str`/`from_sql_str` helpers.
- `reason` is new. The current markdown ledger loses failure reasons. Capturing them here is a latent quality win; cortex can query for recurring failure domains.
- No foreign keys, no joins inside the ledger table. This is an event log. `supersedes` is a soft pointer (TEXT) rather than a FK so orphaned chains don't break inserts.

All v1 dashboard sections are satisfied by pure SQL against `ledger`. The five current Dataview blocks (Today/Yesterday/Week/Month/Stats) all reduce to indexed queries over `created_at`, `status`, `method`, and `domain`. No vault-frontmatter scan is required in this design. If future dashboard sections need current vault state (e.g., an "untriaged" or "unread" view that depends on per-note frontmatter), they land in a follow-up design with a `notes` mirror table populated by cortex — borg is not extended with a vault walker.

### CLI Surface

The only external surface is the CLI. No HTTP routes, no plugin integration.

```
borg ledger [--limit N] [--status S] [--method M] [--domain D] [--since YYYY-MM-DD]
borg ledger --format json            # machine-readable for piping / scripting
borg ledger reconcile                # Phase 3: compare SQLite rows to borg-ledger.md
borg dashboard render                # regenerate snapshot markdown files now
borg export ledger --to-markdown PATH  # rollback helper: reconstruct monolithic markdown from SQLite
borg migrate ledger-md-to-sqlite     # one-shot: import existing borg-ledger.md
```

The background tick (inside the running daemon) calls the same rendering code as `borg dashboard render`; the CLI subcommand is a thin wrapper so the user can force a refresh without restarting the daemon.

Oracle MCP and cortex reach the same data by opening the DB file directly — no intermediate surface.

### Implementation Plan

#### Phase 1: vault::db module + schema
**Model:** sonnet

- Add `tokio-rusqlite` (with `bundled-sqlite` feature) and `rusqlite_migration` to `vault/Cargo.toml` via `cargo add`. `tokio-rusqlite` is the async wrapper around rusqlite; it runs each `Connection` on a dedicated background thread fed via channel, so DB calls are `.await`-able and never block a tokio worker. See "Async Boundaries" below for the rationale.
- Create `vault/src/db.rs` as the module entry (2018+ style) with a `db/` submodule directory.
  - `db/open.rs`: `open(path) -> Result<Connection>`, enables WAL mode, foreign_keys on, sets 0600 permissions on first create. Returns a `tokio_rusqlite::Connection`.
  - `db/migrate.rs`: applies schema versions idempotently, reads `schema_version` table to resume.
  - `db/ledger.rs`: `insert(&conn, &LedgerEntry) -> Result<()>`, `query_since`, `query_by_domain`, `query_by_status`, `count_by_method`, `count_by_domain`, `find_prior_for_retry(canonical_url, window_secs)` (powers the `supersedes` lookup), etc. All queries parameterized; all functions `async`, internally `conn.call(|c| { ... }).await`.
- Define a `DbLedgerEntry` struct that maps 1:1 to the row; convert from `vault::ledger::LedgerEntry`. In the same pass, extend `vault::ledger::LedgerEntry` with `supersedes: Option<String>` so the in-memory type matches the persisted row end-to-end.
- Unit tests against `:memory:` SQLite with fixture rows (tokio-rusqlite supports in-memory).

#### Phase 2: migration subcommand
**Model:** sonnet

- `borg migrate ledger-md-to-sqlite`:
  - Parse `system/views/borg-ledger.md` via existing code in `vault/src/ledger.rs`.
  - Open `~/.local/share/borg/ledger.db`, apply schema migrations.
  - For each parsed row, synthesize a `trace_id` if missing (legacy rows). Format: `<method-prefix>-<6 hex chars of sha256(canonical_url || '\n' || created_at_unix)>`. The hash is stable across re-runs, so a migration re-invocation produces the same trace_id for the same markdown row and `INSERT OR IGNORE` dedupes cleanly. Method prefix follows the staged-pipeline convention (`cl-`, `tg-`, `ds-`, `nt-`, `ht-`, `cb-`).
  - `INSERT OR IGNORE` every row. Populate `supersedes` by running the same 7-day-window lookup the live writer uses; legacy rows thus inherit retry markers where applicable.
  - Log counts: parsed, inserted, skipped (duplicate trace_id), marked-as-retry.
  - Exit non-zero if insert count + skip count != parsed count.
- Idempotent: safe to run repeatedly; on re-run all rows are duplicates and skipped.
- Preserves the markdown file; migration is non-destructive.

#### Phase 3: dual-writer with markdown mirror
**Model:** opus

- In `vault::ledger::append_ledger_entry`, write to SQLite first; on success, optionally also append to the markdown file.
- **Retry/supersedes detection** runs inside the SQLite insert path, not at the pipeline level: before inserting, call `db::ledger::find_prior_for_retry(canonical_url, retry_window_days * 86400)` which returns `Option<String>` (the prior trace_id, if any). The returned value populates `supersedes` on the new row. This keeps the dedup logic colocated with the store that owns the uniqueness question.
- **Duplicate-check migrates to SQLite this phase.** The existing `vault::ledger::check_duplicate` currently reads the markdown file to decide whether a URL has already been ingested. Rewrite it as an indexed SELECT on `ledger.source_url` with `status='completed'`. Once Phase 3 ships, the markdown file is **write-only** — no code reads it on the hot path. This avoids growing the markdown file becoming a read-path performance tax the way it is today, and means the only consumer that still parses the markdown is the reconcile helper.
- Config flags in `borg.yml`:
  ```yaml
  ledger:
    sqlite-enabled: true           # v1: default true
    markdown-mirror: true          # v1: default true; v2: default false; v3: remove the flag
    retry-window-days: 7           # window for supersedes detection; same canonical URL within N days is a retry/refresh
  ```
- Failure semantics: if SQLite insert fails, log `error!` but do not fail the ingest. The ledger is observational. Retain the markdown append as a fallback while mirror is on.
- **`borg ledger reconcile` semantics** (runs weekly during dual-write, gates Phase 5):
  - Parses `borg-ledger.md` and compares against SQLite by trace_id.
  - **Read-only.** Reconcile never writes. If it finds drift, the user's remedy is to re-run `borg migrate ledger-md-to-sqlite` (idempotent, picks up the missing rows).
  - SQLite-has-rows-markdown-doesn't → expected during dual-write (new rows land in SQLite first), logged at `debug!`, exit 0.
  - Markdown-has-rows-SQLite-doesn't → unexpected (SQLite should be the superset). Logged at `warn!`, exit non-zero. CI gate for Phase 5 requires a clean run.
  - Per-trace mismatches (different fields for the same trace_id) → logged at `warn!`, exit non-zero, first 10 offending trace_ids printed.
- File-lock path in `append_ledger_entry` runs only when `markdown-mirror` is on. The fs2 `flock` call is synchronous; when invoked from the existing tokio ingest pipeline, the existing `spawn_blocking` wrapper pattern already in `borg/src/pipeline.rs` applies. When `markdown-mirror` flips to false, the fs2 lock path is dead-code and removed.

#### Phase 4: snapshot renderer
**Model:** opus

- `borg dashboard render`:
  - Reads fixed-set section definitions (today/yesterday/week/month/stats).
  - For each, runs the SQL against `ledger`. No vault scanning.
  - Renders markdown per section via a small templating helper or `format!`. One table per section. Rows where `supersedes IS NOT NULL` are prefixed with `♻️` in the Title column.
  - Preserves `borg-dashboard.md` frontmatter (same title/type/domain/tags as today); replaces only the body between frontmatter and EOF.
  - Writes atomically: `tmp` file + `rename`.
  - Skips the write if the new body is byte-identical to the previous body (prevents Obsidian Sync thrashing and vault git churn).
- `borg ledger --write-to PATH`: same atomic write pattern for `borg-ledger.md`, rendering the most recent N rows (configurable, default 200) as a markdown table. Same ♻️ marker on retry rows.
- Scheduling inside the borg daemon:
  - **Shape:** a single long-lived render task owns the rendering; the ingest path and safety timer both notify it. Concrete plumbing: a `tokio::sync::Notify` (or a 1-capacity `mpsc` with drop-oldest semantics) wakes the task. The task runs a render, then checks the notify flag once more before sleeping — any notifications that arrived mid-render cause exactly one trailing render, no matter how many ingests piled up.
  - **Post-ingest hook:** every successful SQLite insert calls `render_notify.notify_one()`. Dashboard + ledger snapshots are regenerated immediately after each new row lands.
  - **Safety tick:** a 300s `tokio::time::interval` also calls `notify_one()`, so rolling-window sections (today/yesterday/week) stay accurate across date boundaries even when ingests are rare.
- **Daily WAL backup** lives alongside the render task: a `tokio::time::interval` with a 24h period runs `sqlite3 .backup` (via `Connection::backup` in rusqlite, no shell out) to `~/.local/share/borg/backups/ledger-YYYYMMDD.db`. Retains the last 7 daily snapshots, rotates older ones out. This addresses the WAL-corruption risk row in the Risks table.
- CLI: user can always run `borg dashboard render` manually.

#### Phase 5: retire markdown writer
**Model:** sonnet

- Flip `markdown-mirror` default to false in `borg.yml` shipped config.
- Remove the markdown-append code path from `append_ledger_entry` (one release after the flag default flips).
- Retain `vault/src/ledger.rs` markdown parser for one more release in case a user re-runs the migration on a backup; after that, archive it as a module gated behind a `migration-only` feature flag or delete it.
- `system/views/borg-ledger.md` is now exclusively the snapshot target. Nothing to change for the user beyond the file no longer being hand-appended.

#### Phase 6: tests, docs, rollback
**Model:** sonnet

- CLI smoke: ingest a URL, `borg ledger --limit 1` returns it; `borg dashboard render` produces the expected snapshot.
- Golden-markdown tests for the renderer: fixture rows in in-memory SQLite, assert exact markdown output (table bytes stable across runs).
- Migration integration test: fixture `borg-ledger.md` in tmp vault, run migration, compare row counts.
- Reconcile integration test: intentionally diverge the markdown mirror from SQLite, assert `borg ledger reconcile` detects and reports it.
- Post-ingest hook + debounce test: fire 10 concurrent inserts, assert exactly one snapshot file is visible on disk at the end and that its content reflects all 10 rows.
- Rollback: `borg export ledger --to-markdown PATH` reconstructs the monolithic markdown from SQLite, so the vault can revert to pre-SQLite state if the rollout needs to be unwound.

## Alternatives Considered

### Alternative 1: Shard markdown by month, no SQLite
- **Description:** Split `borg-ledger.md` into `ledger/2026-04.md`, `2026-03.md`, etc. Borg appends to the current-month file.
- **Pros:** Zero new infrastructure, one-hour change. Removes the 664KB single-render problem.
- **Cons:** Dataview still O(vault-size) on every dashboard open; no rich query surface; oracle still can't answer ledger questions; retention remains manual; file-lock contention returns once the current month's file grows.
- **Why not chosen:** Treats the symptom, not the cause. Performance still degrades as note count grows.

### Alternative 2: Add LIMIT to every dataview block
- **Description:** Truncate dashboard queries with aggressive `LIMIT` clauses.
- **Pros:** Trivial.
- **Cons:** Dataview still scans the full vault to compute the sort before truncating. Render latency drops only because the output table is smaller, not because the scan is cheaper.
- **Why not chosen:** Doesn't scale past a few thousand notes.

### Alternative 3: Replace Dataview with Obsidian Bases
- **Description:** Use the native Bases feature for dashboard-style views.
- **Pros:** First-party; faster than Dataview at rendering.
- **Cons:** Still vault-frontmatter-only; still scans the whole vault on each view open; doesn't help with the 664KB ledger file; no query surface for CLI or oracle; research confirms no external data source hooks.
- **Why not chosen:** Doesn't solve the core problem (Dataview-like scan cost) or the ledger-size problem.

### Alternative 4: Install a third-party SQLite plugin (stfrigerio/sqliteDB)
- **Description:** Point an existing Obsidian plugin at `ledger.db` directly.
- **Pros:** No borg HTTP surface required; plugin handles rendering.
- **Cons:** Plugin is 36-star personal project, minimal maintenance; couples the vault to raw schema (plugin queries need to know table layout); no access control layer; rendering and query performance depend on plugin code quality.
- **Why not chosen:** Third-party dependency risk is disproportionate to the savings. A 200-line borg route handler beats a 36-star plugin on every axis except "no work."

### Alternative 5: Snapshot-only with a dual-track HTTP fallback
- **Description:** Earlier revision of this design shipped both the snapshot renderer *and* a `GET /views/...` HTTP surface served by axum, intended for an APIRequest-plugin block inside dashboard notes.
- **Pros:** HTTP path gives "always fresh on every note open" without waiting for a tick.
- **Cons:** The HTTP surface binds loopback and is unreachable from Obsidian Sync'd mobile clients; the user's primary multi-device workflow is phone + desktop, making the live path a desktop-only toy.
- **Why not chosen:** Once the snapshot gains a post-ingest hook, "freshness" converges to a few seconds across all devices via Sync, with no plugin dependency. The dual-track complexity (two rendering paths, pool, error-block rendering, plugin docs) paid for a capability that doesn't serve the actual use case. Dropped in favor of snapshot-only.

### Alternative 6: Live-HTTP-only via APIRequest plugin
- **Description:** SQLite + HTTP endpoints + `apirequest` code blocks in dashboard notes, no snapshot.
- **Pros:** Cleanest architecture; no background vault writer; markdown in vault is purely reference.
- **Cons:** Requires APIRequest plugin as a hard dependency; breaks when daemon is down; desktop-only (loopback unreachable from phone); no fallback if the plugin goes unmaintained; mobile dashboard is permanently useless.
- **Why not chosen:** Mobile-hostile. The primary reading workflow includes the phone, so any design that renders broken blocks there is a non-starter.

### Alternative 7: Write a custom Obsidian plugin
- **Description:** 200-LOC Obsidian plugin that registers a `borg-view` code block processor, uses `requestUrl`, renders inline from the local HTTP endpoints.
- **Pros:** Full control over rendering; no APIRequest plugin dependency.
- **Cons:** Same mobile-hostile problem as alternative 6 — the plugin still fetches from localhost. Plus one more codebase to own.
- **Why not chosen:** Doesn't solve the mobile problem. If a desktop-only live panel ever becomes interesting, a TUI or a dedicated plugin can be built on top of the SQLite store in a future design; it doesn't belong in v1.

## Technical Considerations

### Dependencies

- `tokio-rusqlite` (bundled-sqlite feature) for the DB layer. Wraps rusqlite with an async boundary that runs each connection on a dedicated background thread, so callers `.await` without per-call `spawn_blocking`. Used by the borg writer and by CLI/oracle/cortex reader processes (each opens its own single connection).
- `rusqlite_migration` for schema evolution (or hand-rolled if the crate feels heavy).
- No new Obsidian plugin dependencies. No HTTP route dependencies. No pool dependencies.
- No existing dependency changes. Tokio, serde, eyre, log are already present.

### Performance

- 1042 current rows compile to ~100KB in SQLite. Any indexed query returns in <1ms.
- Projected volume: 50k rows over five years is still sub-10ms for dashboard queries.
- Markdown rendering of a 200-row table: <10ms in Rust.
- `supersedes` lookup on insert: single indexed SELECT on `source_url` + `created_at`, sub-ms.
- Dataview today: multiple seconds for the full dashboard load; grows linearly. Expected speedup: one to two orders of magnitude.

### Async Boundaries

`rusqlite` and `fs2` are synchronous. Neither may block a tokio worker thread inside pipeline futures — doing so stalls the reactor for every other concurrent ingest. The design handles this with two patterns:

- **DB calls:** use `tokio-rusqlite`. Each `Connection` runs on a dedicated background thread fed via channel, so callers write `conn.call(|c| { ... }).await` and the sync/async boundary is handled once at the connection type. This applies to the borg writer, the post-ingest render path, the CLI, and oracle/cortex readers — the same idiom everywhere.
- **Process exec and fs2 flock:** keep the existing `tokio::task::spawn_blocking` pattern already in `borg/src/pipeline.rs:550,559,786` and `borg/src/stages/fetcher.rs:149,232`. These are one-off calls around external processes (markitdown, fabric, yt-dlp) and the markdown-mirror fs2 lock — few enough sites that per-call `spawn_blocking` stays ergonomic.

Net: one async wrapper for DB calls, the existing `spawn_blocking` pattern for exec/flock.

### Security

- DB file permissions: 0600 (user-only), set on first create.
- No HTTP surface. No remote surface. The only process boundaries the ledger crosses are other local user processes (oracle MCP, cortex daemon), each of which opens the DB file directly with normal filesystem permissions.
- All queries parameterized internally.
- Ledger data sensitivity: URLs the user has ingested. The snapshot markdown inherits whatever privacy properties the vault itself has (Obsidian Sync is end-to-end encrypted; Syncthing is LAN-only or relay-encrypted — either is acceptable).

### Testing Strategy

- Unit tests for `vault::db` against in-memory SQLite with fixture rows. No mocks.
- Migration integration test: tmp vault with a fixture `borg-ledger.md`, run the migration, compare counts and row contents to the expected set.
- Golden-markdown tests for the renderer: given fixture SQL rows, assert exact markdown output (byte-stable across runs so diffs are obvious when the template changes).
- Reconcile test: intentional SQLite/markdown drift must be detected by `borg ledger reconcile`.
- End-to-end via the existing `bin/e2e` harness: ingest → assert row present in SQLite → assert dashboard snapshot file on disk contains it.
- No mocks of the database; use real SQLite throughout.

### Rollout Plan

1. Phase 1-3 ship together. SQLite exists; dual-write is on; the hand-appended markdown ledger still reflects truth.
2. Run for one to two weeks. `borg ledger reconcile` must report zero drift before advancing.
3. Phase 4 (snapshot renderer) ships. `borg-dashboard.md` and `borg-ledger.md` are now regenerated; the user's dashboard opens fast on desktop and shows up on mobile via Obsidian Sync. Markdown mirror from Phase 3 stays on as a belt-and-suspenders check.
4. Phase 5 (retire markdown writer) ships after a week of Phase 4 stability. The `markdown-mirror` flag default flips to false, the dual-write path is removed.
5. Phase 6 (tests, docs, rollback): the reconcile subcommand and `borg export ledger --to-markdown` stay in the binary so the whole thing remains reversible after ship.
6. Each phase is independently revertible by flag or `borg export ledger --to-markdown`.

## Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| SQLite WAL corruption on crash | Low | Med | WAL mode + daily `sqlite3 .backup` to `~/.local/share/borg/backups/` in the daemon tick |
| Migration misses rows due to markdown parser edge case | Med | Med | Dual-write for at least one release; `borg ledger reconcile` must pass before advancing; keep monolithic markdown in place until verified |
| User renames a note in Obsidian, `filename` column goes stale | Med | Low | `filename` is a hint, not a join key; renderers resolve `trace_id → vault file` via frontmatter lookup. A future cortex rename-watcher can update the column opportunistically. |
| Dual-writer drift between SQLite and markdown during Phase 3 | Med | Med | `borg ledger reconcile` subcommand parses the markdown and compares counts + trace_ids to SQLite; run weekly during dual-write, must pass before Phase 5 flips the default |
| `supersedes` detection mis-marks legitimate fresh captures as retries | Low | Low | 7-day default window is short enough that genuine refreshes of old content are not marked; window is configurable per user taste |
| Obsidian Sync thrashes on snapshot writes | Low | Low | Skip write if body byte-identical; atomic rename; post-ingest hook + 300s tick is infrequent and content-gated, so idle periods produce zero file churn |
| Snapshot markdown grows past Obsidian Sync's 1GB total-vault budget | Low | Low | `borg-ledger.md` is capped at 200 rows (~20KB); `borg-dashboard.md` is bounded by rolling-window sections (~tens of KB). Neither grows unboundedly. If the broader vault does outgrow the budget, switch sync to Syncthing (user already has it ready between Ubuntu machines + Pixel). |
| Cortex later wants to write ledger rows (quality flags) | Med | Low | Hold the line: borg is the only writer. Cortex emits quality events to its own table in the same DB, not to `ledger` |
| Schema migration breaks (future) | Low | Med | `rusqlite_migration` enforces up-only migrations; add columns/tables, never drop; test migrations forward from v1 in CI |
| Inbound `[[borg-ledger]]` wikilinks break if the file is deleted | Low | Low | The file is never deleted, only its content changes. Wikilinks remain valid. |

## Open Questions

- [ ] Do we ever want a `notes` mirror table (for future dashboard sections that depend on current vault state rather than ingest-time state)? (Leaning: defer to its own design. v1's five sections are all ledger-scoped; cortex is the right writer if/when we do this, not borg.)
- [ ] Should `reason` be exposed in the snapshot ledger render, or remain SQL-only for quality analysis? (Leaning: add a narrow `Reason` column to the snapshot only when `status != completed`.)
- [ ] `retry-window-days` default — 7 is the current pick. Worth revisiting after a month of real ingests. A longer window catches more refreshes but risks marking genuine re-discoveries as retries.
- [ ] Safety-tick interval — 300s is the current pick. Short enough that date boundaries (e.g. midnight flipping "today" to "yesterday") are handled within a few minutes; long enough that idle desktop + no-ingest periods produce no spurious writes.

## References

- Current ledger implementation: [`vault/src/ledger.rs`](../../vault/src/ledger.rs), [`borg/src/ledger.rs`](../../borg/src/ledger.rs)
- Current dashboard implementation: [`borg/src/dashboard.rs`](../../borg/src/dashboard.rs)
- Dashboard user view: `obsidian/system/views/borg-dashboard.md`
- Ledger user view: `obsidian/system/views/borg-ledger.md`
- Prior phase: [`docs/design/2026-04-19-staged-ingestion-pipeline.md`](./2026-04-19-staged-ingestion-pipeline.md)
