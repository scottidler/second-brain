//! Receipts: borg's durable record of every input it ever sees.
//!
//! Every front door (`routes.rs`, `telegram.rs`, `discord.rs`, `ntfy.rs`,
//! CLI) calls [`record_received`] synchronously before any pipeline work
//! runs. The pipeline's terminal sites call [`mark_succeeded`] or
//! [`mark_failed`]. The watchdog SELECTs stale rows via [`list_stale`],
//! filters them through the active-permit set, then calls
//! [`promote_single_to_crashed`] for each survivor.
//!
//! State machine:
//!
//! ```text
//! received ──success──> succeeded
//!     │
//!     ├─pipeline error──> failed (stage: fetch-failed / quality-blocked / ...)
//!     │
//!     └─watchdog past deadline──> failed (stage: crashed)
//! ```
//!
//! `succeeded` and `failed` are absorbing states. Every UPDATE includes
//! `WHERE ... AND status='received'` so concurrent transitions cannot stomp
//! each other; SQLite serializes write transactions at the C level via
//! `busy_timeout`.

use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use eyre::{Context, Result, eyre};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, OptionalExtension, params};
use vault::receipts::{FailureStage, ReceiptKind, ReceiptStatus, receipts_db_path, receipts_dir};
use vault::schema::Method;

/// Connection pool size used by the daemon. Covers worst-case concurrent
/// holders: `HEAVY_PERMITS` pipelines + 1 watchdog + 1 front-door receiver
/// + headroom.
pub const POOL_SIZE: u32 = 8;

/// Schema version recorded in the `schema_version` table. Bump when the
/// schema changes.
pub const SCHEMA_VERSION: i64 = 4;

const SCHEMA_SQL: &str = include_str!("receipts/schema.sql");

/// The fixed-width UTC timestamp format used for every `received_at` /
/// `terminal_at` value. Because the column is compared lexicographically in
/// SQL (`received_at >= ?`), any value bound against it MUST be produced with
/// this exact format or string ordering diverges from chronological ordering.
const TIMESTAMP_FMT: &str = "%Y-%m-%dT%H:%M:%SZ";

/// `failure_reason` recorded by [`promote_single_to_crashed`] when the row it
/// reaped carried a lease that had expired (as opposed to a row that never
/// held one, which keeps the generic "no terminal event within Ns" reason).
/// Lets `sb borg log` distinguish a cross-process lease reap from a bare
/// permit-less timeout.
const LEASE_EXPIRED_REASON: &str = "lease-expired";

/// One row from the `receipts` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub trace_id: String,
    pub received_at: String,
    pub method: String,
    pub kind: String,
    pub raw_input: String,
    pub status: String,
    pub terminal_at: Option<String>,
    pub note_path: Option<String>,
    pub failure_stage: Option<String>,
    pub failure_reason: Option<String>,
    pub replay_of: Option<String>,
    /// True when the note was published from a distill fallback (degraded).
    pub degraded: bool,
}

/// Filter for [`query`]. Empty filter means "all rows".
#[derive(Debug, Default, Clone)]
pub struct Filter {
    pub status: Option<ReceiptStatus>,
    pub method: Option<Method>,
    pub stage: Option<FailureStage>,
    /// ISO-8601 string; rows with `received_at >= since` are kept.
    pub since: Option<String>,
    /// SQL LIKE pattern matched against `raw_input`.
    pub source_like: Option<String>,
    /// When `Some(true)`, keep only degraded (distill-fallback) publishes.
    pub degraded: Option<bool>,
    /// Maximum number of rows to return; `None` means no limit.
    pub limit: Option<usize>,
}

/// Connection customizer applied to every connection r2d2 creates. Ensures
/// the four mandatory PRAGMAs are set; WAL mode is per-database (sticks
/// across opens) but `busy_timeout` is per-connection, so the customizer is
/// the only place these are guaranteed.
#[derive(Debug)]
struct PragmaSetter;

impl r2d2::CustomizeConnection<Connection, rusqlite::Error> for PragmaSetter {
    fn on_acquire(&self, conn: &mut Connection) -> std::result::Result<(), rusqlite::Error> {
        apply_pragmas(conn)
    }
}

fn apply_pragmas(conn: &Connection) -> std::result::Result<(), rusqlite::Error> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "busy_timeout", 5000_i64)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create receipts DB parent directory {}", parent.display()))?;
    }
    Ok(())
}

/// Open (and create if absent) the receipts database at the canonical path
/// returned by [`vault::receipts::receipts_db_path`]. Applies the four
/// mandatory PRAGMAs and runs the schema migration idempotently.
pub fn open_default() -> Result<Connection> {
    let path = receipts_db_path()?;
    let dir = receipts_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("Failed to create receipts directory {}", dir.display()))?;
    open_at(&path)
}

/// Open the receipts DB at an explicit path. Used by tests and migration
/// tooling that need to operate on a non-default location.
pub fn open_at(path: &Path) -> Result<Connection> {
    log::debug!("receipts::open_at: path={}", path.display());
    ensure_parent_dir(path)?;
    let conn = Connection::open(path).with_context(|| format!("Failed to open receipts DB at {}", path.display()))?;
    apply_pragmas(&conn).context("Failed to apply receipts PRAGMAs")?;
    run_migrations(&conn).context("Failed to apply receipts schema migrations")?;
    Ok(conn)
}

/// Open an in-memory receipts DB (for tests). PRAGMAs and schema applied.
pub fn open_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory().context("Failed to open in-memory receipts DB")?;
    apply_pragmas(&conn).context("Failed to apply receipts PRAGMAs (in-memory)")?;
    run_migrations(&conn).context("Failed to apply receipts schema migrations (in-memory)")?;
    Ok(conn)
}

/// Build an r2d2 pool over the canonical receipts DB path. Used by the
/// daemon. Each pooled connection has the four PRAGMAs applied via
/// `PragmaSetter`.
pub fn build_pool() -> Result<Pool<SqliteConnectionManager>> {
    let path = receipts_db_path()?;
    build_pool_at(&path)
}

/// Build an r2d2 pool at an explicit path. Tests use this to put the pool
/// over a tempdir.
pub fn build_pool_at(path: &Path) -> Result<Pool<SqliteConnectionManager>> {
    log::debug!("receipts::build_pool_at: path={} size={}", path.display(), POOL_SIZE);
    ensure_parent_dir(path)?;
    // Initialize the file and run migrations once via a one-shot open, so
    // pool checkouts find an already-migrated DB.
    let _bootstrap = open_at(path)?;
    drop(_bootstrap);
    let manager = SqliteConnectionManager::file(path);
    let pool = Pool::builder()
        .max_size(POOL_SIZE)
        .connection_customizer(Box::new(PragmaSetter))
        .build(manager)
        .with_context(|| format!("Failed to build receipts pool at {}", path.display()))?;
    Ok(pool)
}

/// Whether `table` already has a column named `column`. Used to make
/// ADD-COLUMN migrations idempotent (a fresh DB created from the baseline
/// schema already has the column; an old DB does not).
fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .context("prepare table_info")?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .context("query table_info")?;
    for n in names {
        if n.context("read table_info row")? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn run_migrations(conn: &Connection) -> Result<()> {
    conn.execute_batch(SCHEMA_SQL).context("schema batch")?;
    // v2: add the `degraded` column to pre-existing DBs. A fresh DB already
    // has it (baseline schema); old rows default to 0. Idempotent via the
    // column probe, so re-running open() is safe.
    if !has_column(conn, "receipts", "degraded")? {
        conn.execute(
            "ALTER TABLE receipts ADD COLUMN degraded INTEGER NOT NULL DEFAULT 0",
            [],
        )
        .context("add degraded column")?;
    }
    // v3 (harvest-clyde-sessions design, Phase 1): widen the `kind` and
    // `status` CHECK constraints to add 'session' and 'rejected'. SQLite has
    // no ALTER-CHECK-CONSTRAINT, so this rebuilds the table: create a copy
    // with the widened constraints, copy every row, drop the old table,
    // rename the copy into place. Idempotency is probed directly against the
    // live table definition (not the schema_version counter) so this is a
    // no-op both on a DB already migrated to v3 AND on a brand new DB whose
    // baseline `schema.sql` was created with the widened constraint already.
    let receipts_sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='receipts'",
            [],
            |row| row.get(0),
        )
        .context("read receipts table definition")?;
    if !receipts_sql.contains("'rejected'") {
        conn.execute_batch(
            "CREATE TABLE receipts_v3 (
               trace_id        TEXT NOT NULL PRIMARY KEY,
               received_at     TEXT NOT NULL,
               method          TEXT NOT NULL,
               kind            TEXT NOT NULL
                                CHECK (kind IN ('url', 'text', 'binary', 'session')),
               raw_input       TEXT NOT NULL,
               status          TEXT NOT NULL
                                CHECK (status IN ('received', 'succeeded', 'failed', 'rejected')),
               terminal_at     TEXT,
               note_path       TEXT,
               failure_stage   TEXT
                                CHECK (failure_stage IS NULL OR failure_stage IN (
                                  'intake-rejected', 'classify-failed', 'fetch-failed',
                                  'quality-blocked', 'pipeline-timed-out', 'publish-failed',
                                  'crashed'
                                )),
               failure_reason  TEXT,
               replay_of       TEXT,
               degraded        INTEGER NOT NULL DEFAULT 0
             );
             INSERT INTO receipts_v3
               (trace_id, received_at, method, kind, raw_input, status,
                terminal_at, note_path, failure_stage, failure_reason, replay_of, degraded)
               SELECT trace_id, received_at, method, kind, raw_input, status,
                      terminal_at, note_path, failure_stage, failure_reason, replay_of, degraded
               FROM receipts;
             DROP TABLE receipts;
             ALTER TABLE receipts_v3 RENAME TO receipts;
             CREATE INDEX IF NOT EXISTS idx_receipts_status ON receipts(status);
             CREATE INDEX IF NOT EXISTS idx_receipts_received_at ON receipts(received_at);
             CREATE INDEX IF NOT EXISTS idx_receipts_method_status ON receipts(method, status);",
        )
        .context("rebuild receipts table for v3 CHECK constraint widen")?;
    }
    // v4 (harvest-watchdog-cross-process-reaping design, Phase 1): add the
    // shared trace-lease columns (`lease_owner_pid`, `lease_until`) so the
    // daemon watchdog and a separate `sb borg harvest` process can agree on
    // trace liveness through the shared receipts row instead of the
    // process-local `ACTIVE_TRACES` set. MUST run AFTER the v3 rebuild block
    // above: that rebuild's fixed 12-column `INSERT...SELECT` would silently
    // DROP any column added before it runs, on a pre-v3 DB. Idempotent via
    // the column probe, same pattern as the v2 `degraded` add.
    if !has_column(conn, "receipts", "lease_owner_pid")? {
        log::debug!("receipts::run_migrations: adding lease_owner_pid column (v4)");
        conn.execute(
            "ALTER TABLE receipts ADD COLUMN lease_owner_pid INTEGER DEFAULT NULL",
            [],
        )
        .context("add lease_owner_pid column")?;
    }
    if !has_column(conn, "receipts", "lease_until")? {
        log::debug!("receipts::run_migrations: adding lease_until column (v4)");
        conn.execute("ALTER TABLE receipts ADD COLUMN lease_until TEXT DEFAULT NULL", [])
            .context("add lease_until column")?;
    }
    // MAX() over an empty table returns one row whose value is NULL, which
    // rusqlite cannot decode straight into i64; bind through Option to read
    // it cleanly.
    let current: Option<i64> = conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
            row.get::<_, Option<i64>>(0)
        })
        .context("read schema_version")?;
    // Rollback safety: a stored version >= the code's SCHEMA_VERSION (e.g. this
    // DB was already opened by a newer binary, or a future migration lands
    // ahead of this one in a rollback) is a no-op, never a downgrade and never
    // a panic. The column/table probes above are idempotent regardless of this
    // counter, so an older binary opening a newer DB still gets its additive
    // columns/tables verified present; it just leaves `schema_version` alone.
    match current {
        Some(v) if v >= SCHEMA_VERSION => Ok(()),
        _ => {
            conn.execute(
                "INSERT OR IGNORE INTO schema_version (version, applied_at) VALUES (?, ?)",
                params![SCHEMA_VERSION, now_iso8601()],
            )
            .context("insert schema_version row")?;
            Ok(())
        }
    }
}

fn now_iso8601() -> String {
    Utc::now().format(TIMESTAMP_FMT).to_string()
}

/// Parse a `--since` value into an absolute UTC timestamp string in the same
/// format the receipts table stores (`TIMESTAMP_FMT`), so it can be bound
/// directly into the `received_at >= ?` comparison. Accepts three forms:
///
///   - relative duration measured back from `now` (`5m`, `2h`, `7d`, `1h30m`)
///   - absolute RFC-3339 / ISO-8601 datetime (`2026-06-04T05:18:59Z`, or with
///     an offset, normalized to UTC)
///   - bare calendar date (`2026-06-04`, interpreted as `00:00:00Z`) - this is
///     what `sb borg log --since "$(date -I ...)"` produces
///
/// Returns a loud error on anything it cannot parse. The old behavior bound
/// the raw string verbatim, so a relative duration like `5m` was compared
/// lexicographically against ISO timestamps (`"2026-..." >= "5m"` is always
/// false) and silently excluded every row - masquerading as "no data".
pub fn parse_since(input: &str, now: DateTime<Utc>) -> Result<String> {
    let trimmed = input.trim();
    log::debug!("receipts::parse_since: input={trimmed}");
    if let Ok(dur) = humantime::parse_duration(trimmed) {
        let chrono_dur =
            chrono::Duration::from_std(dur).map_err(|e| eyre!("--since duration {trimmed:?} out of range: {e}"))?;
        let cutoff = now
            .checked_sub_signed(chrono_dur)
            .ok_or_else(|| eyre!("--since duration {trimmed:?} overflows the representable range"))?;
        return Ok(cutoff.format(TIMESTAMP_FMT).to_string());
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Ok(dt.with_timezone(&Utc).format(TIMESTAMP_FMT).to_string());
    }
    if let Ok(date) = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        let midnight = date.and_hms_opt(0, 0, 0).expect("00:00:00 is a valid time");
        return Ok(Utc.from_utc_datetime(&midnight).format(TIMESTAMP_FMT).to_string());
    }
    Err(eyre!(
        "could not parse --since {trimmed:?}: expected a relative duration (e.g. 5m, 2h, 7d), \
         an ISO-8601 datetime (e.g. 2026-06-04T05:18:59Z), or a date (e.g. 2026-06-04)"
    ))
}

/// INSERT a `received` row at the door. Idempotent on `trace_id` (INSERT OR
/// IGNORE) so a retried front-door dispatch cannot create a duplicate row.
pub fn record_received(
    conn: &Connection,
    trace_id: &str,
    method: Method,
    kind: ReceiptKind,
    raw_input: &str,
) -> Result<()> {
    record_received_flagged(conn, trace_id, method, kind, raw_input, false)
}

/// Like [`record_received`] but the caller already EXPECTS a row to exist
/// (e.g. the failure-at-door upsert, where the door capture ran first). A
/// no-op insert (`rows == 0`) is then normal and logs at DEBUG instead of
/// WARN - the WARN was spamming on every door rejection.
pub fn record_received_expecting_existing(
    conn: &Connection,
    trace_id: &str,
    method: Method,
    kind: ReceiptKind,
    raw_input: &str,
) -> Result<()> {
    record_received_flagged(conn, trace_id, method, kind, raw_input, true)
}

fn record_received_flagged(
    conn: &Connection,
    trace_id: &str,
    method: Method,
    kind: ReceiptKind,
    raw_input: &str,
    expected_existing: bool,
) -> Result<()> {
    log::debug!(
        "receipts::record_received: trace={} method={} kind={} raw_len={} expected_existing={}",
        trace_id,
        method,
        kind,
        raw_input.len(),
        expected_existing
    );
    let rows = conn
        .execute(
            "INSERT OR IGNORE INTO receipts \
             (trace_id, received_at, method, kind, raw_input, status) \
             VALUES (?, ?, ?, ?, ?, 'received')",
            params![trace_id, now_iso8601(), method.as_str(), kind.as_str(), raw_input],
        )
        .with_context(|| format!("Failed to record received trace_id={trace_id}"))?;
    if rows == 0 {
        if expected_existing {
            log::debug!("receipts::record_received: trace_id={trace_id} already present (expected), no-op");
        } else {
            log::warn!("receipts::record_received: trace_id={trace_id} already present, no-op");
        }
    }
    Ok(())
}

/// INSERT a `received` row tagged as a replay of `original_trace_id`.
pub fn record_replay(
    conn: &Connection,
    trace_id: &str,
    method: Method,
    kind: ReceiptKind,
    raw_input: &str,
    original_trace_id: &str,
) -> Result<()> {
    log::debug!(
        "receipts::record_replay: trace={} replay_of={} method={} kind={}",
        trace_id,
        original_trace_id,
        method,
        kind
    );
    conn.execute(
        "INSERT OR IGNORE INTO receipts \
         (trace_id, received_at, method, kind, raw_input, status, replay_of) \
         VALUES (?, ?, ?, ?, ?, 'received', ?)",
        params![
            trace_id,
            now_iso8601(),
            method.as_str(),
            kind.as_str(),
            raw_input,
            original_trace_id
        ],
    )
    .with_context(|| format!("Failed to record replay trace_id={trace_id} of {original_trace_id}"))?;
    Ok(())
}

/// Promote a `received` row to `succeeded`. The `WHERE status='received'`
/// guard makes this a no-op if the row has already moved to a terminal
/// state (e.g. the watchdog beat the pipeline to it).
pub fn mark_succeeded(conn: &Connection, trace_id: &str, note_path: &str, degraded: bool) -> Result<bool> {
    log::debug!("receipts::mark_succeeded: trace={trace_id} note_path={note_path} degraded={degraded}");
    // The lease clear rides this SAME UPDATE (Resolved Decision: "clear
    // folded into the terminal UPDATE") - no separate happy-path I/O, and no
    // window where a terminal row still carries a live-looking lease.
    let rows = conn
        .execute(
            "UPDATE receipts SET status='succeeded', terminal_at=?, note_path=?, degraded=?, \
             lease_owner_pid=NULL, lease_until=NULL \
             WHERE trace_id=? AND status='received'",
            params![now_iso8601(), note_path, degraded as i64, trace_id],
        )
        .with_context(|| format!("Failed to mark succeeded trace_id={trace_id}"))?;
    if rows == 0 {
        log::warn!("receipts::mark_succeeded: trace_id={trace_id} not in 'received' state (already terminal)");
    }
    Ok(rows > 0)
}

/// Promote a `received` row to `rejected` (the harvest selection gate
/// declining to publish, `GateId::Selection` - harvest-clyde-sessions
/// design). Distinct from [`mark_failed`]: a rejection is the gate correctly
/// declining, not a broken ingest, so it gets its own status rather than
/// riding `failed` with a stage that would lie about what happened.
pub fn mark_rejected(conn: &Connection, trace_id: &str, reason: &str) -> Result<bool> {
    log::debug!("receipts::mark_rejected: trace={trace_id} reason_len={}", reason.len());
    let rows = conn
        .execute(
            "UPDATE receipts SET status='rejected', failure_reason=?, terminal_at=? \
             WHERE trace_id=? AND status='received'",
            params![reason, now_iso8601(), trace_id],
        )
        .with_context(|| format!("Failed to mark rejected trace_id={trace_id}"))?;
    if rows == 0 {
        log::warn!("receipts::mark_rejected: trace_id={trace_id} not in 'received' state (already terminal)");
    }
    Ok(rows > 0)
}

/// Promote a `received` row to `failed` with the given stage and reason.
pub fn mark_failed(conn: &Connection, trace_id: &str, stage: FailureStage, reason: &str) -> Result<bool> {
    log::debug!(
        "receipts::mark_failed: trace={trace_id} stage={stage} reason_len={}",
        reason.len()
    );
    // Lease clear rides this SAME UPDATE, same rationale as mark_succeeded.
    let rows = conn
        .execute(
            "UPDATE receipts SET status='failed', failure_stage=?, failure_reason=?, terminal_at=?, \
             lease_owner_pid=NULL, lease_until=NULL \
             WHERE trace_id=? AND status='received'",
            params![stage.as_str(), reason, now_iso8601(), trace_id],
        )
        .with_context(|| format!("Failed to mark failed trace_id={trace_id} stage={stage}"))?;
    if rows == 0 {
        log::warn!("receipts::mark_failed: trace_id={trace_id} not in 'received' state (already terminal)");
    }
    Ok(rows > 0)
}

/// Write the cross-process liveness lease on a trace's receipts row:
/// `lease_owner_pid` (diagnostic only - PID reuse means it is never the
/// liveness gate) and `lease_until` (the gate itself, [`TIMESTAMP_FMT`]).
/// Guarded to `status='received'` so a lease can never be stamped onto a row
/// that has already reached a terminal state. Called at trace entry
/// (`pipeline.rs`, Phase 4) before the permit is granted, so a permit-queued
/// trace already holds a lease the watchdog must respect.
pub fn write_lease(conn: &Connection, trace_id: &str, pid: u32, lease_until: &str) -> Result<()> {
    log::debug!("receipts::write_lease: trace={trace_id} pid={pid} lease_until={lease_until}");
    let rows = conn
        .execute(
            "UPDATE receipts SET lease_owner_pid=?, lease_until=? \
             WHERE trace_id=? AND status='received'",
            params![pid, lease_until, trace_id],
        )
        .with_context(|| format!("Failed to write lease trace_id={trace_id}"))?;
    if rows == 0 {
        log::warn!("receipts::write_lease: trace_id={trace_id} not in 'received' state, no-op");
    }
    Ok(())
}

/// Re-stamp `lease_until` on an already-leased row (renew at permit grant, so
/// the actual-processing window is measured from when work truly starts).
/// Same `status='received'` guard as [`write_lease`].
pub fn renew_lease(conn: &Connection, trace_id: &str, lease_until: &str) -> Result<()> {
    log::debug!("receipts::renew_lease: trace={trace_id} lease_until={lease_until}");
    let rows = conn
        .execute(
            "UPDATE receipts SET lease_until=? WHERE trace_id=? AND status='received'",
            params![lease_until, trace_id],
        )
        .with_context(|| format!("Failed to renew lease trace_id={trace_id}"))?;
    if rows == 0 {
        log::warn!("receipts::renew_lease: trace_id={trace_id} not in 'received' state, no-op");
    }
    Ok(())
}

/// SELECT the trace_ids that are candidates for crashed-promotion: status
/// `received`, `received_at` older than the deadline, AND no live lease
/// (`lease_until` absent or already expired against `now`). A row whose
/// owning process is still renewing its lease is excluded here even past the
/// `received_at` deadline - it is legitimately mid-flight in a SEPARATE OS
/// process the daemon's own in-memory active-trace set cannot see. `now` is
/// caller-injected so tests are deterministic (no wall-clock read inside).
/// Returned in reverse-chronological order so the watchdog can sample / log.
pub fn list_stale(conn: &Connection, deadline_secs: u64, now: DateTime<Utc>) -> Result<Vec<(String, String)>> {
    let cutoff = now - chrono::Duration::seconds(deadline_secs as i64);
    let cutoff_iso = cutoff.format(TIMESTAMP_FMT).to_string();
    let now_iso = now.format(TIMESTAMP_FMT).to_string();
    let mut stmt = conn
        .prepare(
            "SELECT trace_id, received_at FROM receipts \
             WHERE status='received' AND received_at < ? \
             AND (lease_until IS NULL OR lease_until < ?) \
             ORDER BY received_at DESC",
        )
        .context("prepare list_stale")?;
    let iter = stmt
        .query_map(params![cutoff_iso, now_iso], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .context("query_map list_stale")?;
    let mut out = Vec::new();
    for r in iter {
        out.push(r.context("read list_stale row")?);
    }
    Ok(out)
}

/// Promote a single trace from `received` to `failed` with stage `crashed`.
/// Used by the watchdog after the in-memory permit filter.
///
/// Repeats the SAME lease predicate as [`list_stale`] ATOMICALLY in this
/// UPDATE's `WHERE` clause (`status='received' AND (lease_until IS NULL OR
/// lease_until < now)`) - this is the TOCTOU fix: if the owning process
/// renews between the watchdog's `list_stale` SELECT and this promotion
/// UPDATE, the UPDATE matches 0 rows and the live trace is NOT reaped, even
/// though it was a stale candidate a moment ago. `now` is caller-injected,
/// same value the caller passed to `list_stale`, so the two checks agree.
///
/// A row whose lease was live but expired gets [`LEASE_EXPIRED_REASON`]
/// instead of the generic "no terminal event" reason, so `sb borg log` can
/// tell a cross-process lease reap apart from a bare permit-less timeout.
pub fn promote_single_to_crashed(
    conn: &Connection,
    trace_id: &str,
    deadline_secs: u64,
    now: DateTime<Utc>,
) -> Result<bool> {
    let now_iso = now.format(TIMESTAMP_FMT).to_string();
    let generic_reason = format!("no terminal event within {deadline_secs}s");
    let rows = conn
        .execute(
            "UPDATE receipts SET status='failed', failure_stage='crashed', \
             failure_reason = CASE \
               WHEN lease_until IS NOT NULL AND lease_until < ? THEN ? \
               ELSE ? \
             END, \
             terminal_at = ? \
             WHERE trace_id = ? AND status = 'received' \
             AND (lease_until IS NULL OR lease_until < ?)",
            params![
                now_iso,
                LEASE_EXPIRED_REASON,
                generic_reason,
                now_iso8601(),
                trace_id,
                now_iso
            ],
        )
        .with_context(|| format!("Failed to promote {trace_id} to crashed"))?;
    Ok(rows > 0)
}

/// Look up a single receipts row by trace_id.
pub fn get(conn: &Connection, trace_id: &str) -> Result<Option<Receipt>> {
    let mut stmt = conn
        .prepare(
            "SELECT trace_id, received_at, method, kind, raw_input, status, \
                    terminal_at, note_path, failure_stage, failure_reason, replay_of, degraded \
             FROM receipts WHERE trace_id=?",
        )
        .context("prepare get")?;
    let row = stmt
        .query_row(params![trace_id], row_to_receipt)
        .optional()
        .context("query_row get")?;
    Ok(row)
}

/// Query receipts rows with the given filter. Results are
/// reverse-chronological by `received_at` (newest first).
pub fn query(conn: &Connection, filter: &Filter) -> Result<Vec<Receipt>> {
    let mut sql = String::from(
        "SELECT trace_id, received_at, method, kind, raw_input, status, \
                terminal_at, note_path, failure_stage, failure_reason, replay_of, degraded \
         FROM receipts WHERE 1=1",
    );
    let mut bound: Vec<rusqlite::types::Value> = Vec::new();
    if let Some(status) = filter.status {
        sql.push_str(" AND status=?");
        bound.push(status.as_str().to_string().into());
    }
    if let Some(method) = filter.method {
        sql.push_str(" AND method=?");
        bound.push(method.as_str().to_string().into());
    }
    if let Some(stage) = filter.stage {
        sql.push_str(" AND failure_stage=?");
        bound.push(stage.as_str().to_string().into());
    }
    if let Some(since) = &filter.since {
        sql.push_str(" AND received_at >= ?");
        bound.push(since.clone().into());
    }
    if let Some(pat) = &filter.source_like {
        sql.push_str(" AND raw_input LIKE ?");
        bound.push(pat.clone().into());
    }
    if let Some(degraded) = filter.degraded {
        sql.push_str(" AND degraded=?");
        bound.push((degraded as i64).into());
    }
    sql.push_str(" ORDER BY received_at DESC");
    if let Some(limit) = filter.limit {
        sql.push_str(" LIMIT ?");
        bound.push((limit as i64).into());
    }

    let mut stmt = conn.prepare(&sql).context("prepare query")?;
    let params_iter = rusqlite::params_from_iter(bound.iter());
    let iter = stmt.query_map(params_iter, row_to_receipt).context("query_map query")?;
    let mut out = Vec::new();
    for r in iter {
        out.push(r.context("read query row")?);
    }
    Ok(out)
}

fn row_to_receipt(row: &rusqlite::Row<'_>) -> rusqlite::Result<Receipt> {
    Ok(Receipt {
        trace_id: row.get(0)?,
        received_at: row.get(1)?,
        method: row.get(2)?,
        kind: row.get(3)?,
        raw_input: row.get(4)?,
        status: row.get(5)?,
        terminal_at: row.get(6)?,
        note_path: row.get(7)?,
        failure_stage: row.get(8)?,
        failure_reason: row.get(9)?,
        replay_of: row.get(10)?,
        degraded: row.get::<_, i64>(11)? != 0,
    })
}

/// Read the active PRAGMA values from a connection. Used by tests and the
/// `sb doctor` check to verify open_at applied the four mandatory PRAGMAs.
pub fn active_pragmas(conn: &Connection) -> Result<PragmaSnapshot> {
    let journal_mode: String = conn
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .context("read journal_mode")?;
    let synchronous: i64 = conn
        .pragma_query_value(None, "synchronous", |row| row.get(0))
        .context("read synchronous")?;
    let busy_timeout: i64 = conn
        .pragma_query_value(None, "busy_timeout", |row| row.get(0))
        .context("read busy_timeout")?;
    let foreign_keys: i64 = conn
        .pragma_query_value(None, "foreign_keys", |row| row.get(0))
        .context("read foreign_keys")?;
    Ok(PragmaSnapshot {
        journal_mode,
        synchronous,
        busy_timeout,
        foreign_keys: foreign_keys != 0,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PragmaSnapshot {
    pub journal_mode: String,
    pub synchronous: i64,
    pub busy_timeout: i64,
    pub foreign_keys: bool,
}

/// Resolve the default receipts DB path, ensuring the parent directory exists.
/// Used by callers (front doors, CLI verbs) that want to open the DB without
/// knowing the schema migration details.
pub fn default_path() -> Result<PathBuf> {
    let path = receipts_db_path()?;
    ensure_parent_dir(&path)?;
    Ok(path)
}

/// Convert an open Connection's path-typed error message into something the
/// caller can attach as context. Useful for the audit verb that wants a
/// path-aware error message without re-opening the DB.
pub fn path_for_error(conn: &Connection) -> String {
    conn.path()
        .map(|p| p.to_string())
        .unwrap_or_else(|| "<unknown receipts DB path>".to_string())
}

/// Convenience: open the DB and return both the connection and its resolved
/// path. Used by `sb borg log` and friends that want to print the path in
/// the human header.
pub fn open_default_with_path() -> Result<(Connection, PathBuf)> {
    let path = receipts_db_path()?;
    let conn = open_at(&path)?;
    Ok((conn, path))
}

/// Sanity check the receipts table is non-empty (used in `sb status`).
/// Returns the count of rows in the table.
pub fn row_count(conn: &Connection) -> Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM receipts", [], |row| row.get(0))
        .context("count receipts rows")
}

/// Count receipts by status. Returns `(received, succeeded, failed, rejected)`.
/// `rejected` is the harvest selection gate's outcome (`GateId::Selection`,
/// harvest-clyde-sessions design) - written starting Phase 3, plumbed here in
/// Phase 1 so this aggregate never hard-errors the moment the first rejected
/// row lands.
pub fn count_by_status(conn: &Connection) -> Result<(i64, i64, i64, i64)> {
    let mut stmt = conn
        .prepare("SELECT status, COUNT(*) FROM receipts GROUP BY status")
        .context("prepare count_by_status")?;
    let mut received = 0_i64;
    let mut succeeded = 0_i64;
    let mut failed = 0_i64;
    let mut rejected = 0_i64;
    let iter = stmt
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
        .context("query_map count_by_status")?;
    for r in iter {
        let (status, count) = r.context("read count_by_status row")?;
        match status.as_str() {
            "received" => received = count,
            "succeeded" => succeeded = count,
            "failed" => failed = count,
            "rejected" => rejected = count,
            other => return Err(eyre!("unexpected status value in receipts: {other}")),
        }
    }
    Ok((received, succeeded, failed, rejected))
}

/// Count failed receipts grouped by failure_stage. Returns a list of
/// `(stage, count)` pairs in arbitrary order.
pub fn count_failed_by_stage(conn: &Connection) -> Result<Vec<(FailureStage, i64)>> {
    let mut stmt = conn
        .prepare(
            "SELECT failure_stage, COUNT(*) FROM receipts \
             WHERE status='failed' AND failure_stage IS NOT NULL \
             GROUP BY failure_stage",
        )
        .context("prepare count_failed_by_stage")?;
    let iter = stmt
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
        .context("query_map count_failed_by_stage")?;
    let mut out = Vec::new();
    for r in iter {
        let (stage_str, count) = r.context("read count_failed_by_stage row")?;
        if let Ok(stage) = stage_str.parse::<FailureStage>() {
            out.push((stage, count));
        } else {
            log::warn!("receipts::count_failed_by_stage: unknown failure_stage value {stage_str}");
        }
    }
    Ok(out)
}

/// Count receipts that reached a `failed` terminal state at or after
/// `since_iso` (compared against `terminal_at`, not `received_at` - a row may
/// have been received long before it failed). `since_iso` must be in
/// [`TIMESTAMP_FMT`].
pub fn count_failed_since(conn: &Connection, since_iso: &str) -> Result<i64> {
    log::debug!("receipts::count_failed_since: since={since_iso}");
    conn.query_row(
        "SELECT COUNT(*) FROM receipts WHERE status='failed' AND terminal_at >= ?",
        params![since_iso],
        |row| row.get(0),
    )
    .context("count_failed_since")
}

/// Count receipts promoted to `crashed` at or after `since_iso` (the watchdog's
/// silent-drop signal). Compared against `terminal_at`. `since_iso` must be in
/// [`TIMESTAMP_FMT`].
pub fn count_crashed_since(conn: &Connection, since_iso: &str) -> Result<i64> {
    log::debug!("receipts::count_crashed_since: since={since_iso}");
    conn.query_row(
        "SELECT COUNT(*) FROM receipts WHERE status='failed' AND failure_stage='crashed' AND terminal_at >= ?",
        params![since_iso],
        |row| row.get(0),
    )
    .context("count_crashed_since")
}

/// Count notes published in a degraded state (a distill fallback, e.g. a fabric
/// API error) at or after `since_iso`. A degraded publish still has
/// `status='succeeded'` - the note landed - but `degraded=1` flags that
/// distillation fell back, so the body is impoverished (no summary/claims). This
/// is the silent-quality signal: it never shows up in failed/crashed counts.
/// Compared against `terminal_at`. `since_iso` must be in [`TIMESTAMP_FMT`].
pub fn count_degraded_since(conn: &Connection, since_iso: &str) -> Result<i64> {
    log::debug!("receipts::count_degraded_since: since={since_iso}");
    conn.query_row(
        "SELECT COUNT(*) FROM receipts WHERE status='succeeded' AND degraded=1 AND terminal_at >= ?",
        params![since_iso],
        |row| row.get(0),
    )
    .context("count_degraded_since")
}

/// Count receipts of a given `kind` received at or after `since_iso`
/// (`received_at`, not `terminal_at` - a rejected/still-`received` row still
/// TOUCHED the pipeline in the window even if it never reached a successful
/// terminal state). This is the durable proxy the harvest drift guard (Phase 6
/// of the harvest-completion design) reads: "did the harvest run produce ANY
/// session activity recently", independent of whether that activity ultimately
/// succeeded. `since_iso` must be in [`TIMESTAMP_FMT`].
pub fn count_kind_since(conn: &Connection, kind: ReceiptKind, since_iso: &str) -> Result<i64> {
    log::debug!("receipts::count_kind_since: kind={} since={since_iso}", kind.as_str());
    conn.query_row(
        "SELECT COUNT(*) FROM receipts WHERE kind = ? AND received_at >= ?",
        params![kind.as_str(), since_iso],
        |row| row.get(0),
    )
    .context("count_kind_since")
}

/// Format `now - hours` as a [`TIMESTAMP_FMT`] lower bound for the
/// `*_since` counters.
pub fn hours_ago_iso(hours: i64) -> String {
    (Utc::now() - chrono::Duration::hours(hours))
        .format(TIMESTAMP_FMT)
        .to_string()
}

#[cfg(test)]
mod tests;
