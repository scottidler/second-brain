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
pub const SCHEMA_VERSION: i64 = 2;

const SCHEMA_SQL: &str = include_str!("receipts/schema.sql");

/// The fixed-width UTC timestamp format used for every `received_at` /
/// `terminal_at` value. Because the column is compared lexicographically in
/// SQL (`received_at >= ?`), any value bound against it MUST be produced with
/// this exact format or string ordering diverges from chronological ordering.
const TIMESTAMP_FMT: &str = "%Y-%m-%dT%H:%M:%SZ";

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
    // MAX() over an empty table returns one row whose value is NULL, which
    // rusqlite cannot decode straight into i64; bind through Option to read
    // it cleanly.
    let current: Option<i64> = conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
            row.get::<_, Option<i64>>(0)
        })
        .context("read schema_version")?;
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
    let rows = conn
        .execute(
            "UPDATE receipts SET status='succeeded', terminal_at=?, note_path=?, degraded=? \
             WHERE trace_id=? AND status='received'",
            params![now_iso8601(), note_path, degraded as i64, trace_id],
        )
        .with_context(|| format!("Failed to mark succeeded trace_id={trace_id}"))?;
    if rows == 0 {
        log::warn!("receipts::mark_succeeded: trace_id={trace_id} not in 'received' state (already terminal)");
    }
    Ok(rows > 0)
}

/// Promote a `received` row to `failed` with the given stage and reason.
pub fn mark_failed(conn: &Connection, trace_id: &str, stage: FailureStage, reason: &str) -> Result<bool> {
    log::debug!(
        "receipts::mark_failed: trace={trace_id} stage={stage} reason_len={}",
        reason.len()
    );
    let rows = conn
        .execute(
            "UPDATE receipts SET status='failed', failure_stage=?, failure_reason=?, terminal_at=? \
             WHERE trace_id=? AND status='received'",
            params![stage.as_str(), reason, now_iso8601(), trace_id],
        )
        .with_context(|| format!("Failed to mark failed trace_id={trace_id} stage={stage}"))?;
    if rows == 0 {
        log::warn!("receipts::mark_failed: trace_id={trace_id} not in 'received' state (already terminal)");
    }
    Ok(rows > 0)
}

/// SELECT the trace_ids that are candidates for crashed-promotion: status
/// `received` with `received_at` older than the deadline. Returned in
/// reverse-chronological order so the watchdog can sample / log.
pub fn list_stale(conn: &Connection, deadline_secs: u64) -> Result<Vec<(String, String)>> {
    let now = Utc::now();
    let cutoff = now - chrono::Duration::seconds(deadline_secs as i64);
    let cutoff_iso = cutoff.format(TIMESTAMP_FMT).to_string();
    let mut stmt = conn
        .prepare(
            "SELECT trace_id, received_at FROM receipts \
             WHERE status='received' AND received_at < ? \
             ORDER BY received_at DESC",
        )
        .context("prepare list_stale")?;
    let iter = stmt
        .query_map(params![cutoff_iso], |row| {
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
pub fn promote_single_to_crashed(conn: &Connection, trace_id: &str, deadline_secs: u64) -> Result<bool> {
    let reason = format!("no terminal event within {deadline_secs}s");
    let rows = conn
        .execute(
            "UPDATE receipts SET status='failed', failure_stage='crashed', \
             failure_reason=?, terminal_at=? \
             WHERE trace_id=? AND status='received'",
            params![reason, now_iso8601(), trace_id],
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

/// Count receipts by status. Returns `(received, succeeded, failed)`.
pub fn count_by_status(conn: &Connection) -> Result<(i64, i64, i64)> {
    let mut stmt = conn
        .prepare("SELECT status, COUNT(*) FROM receipts GROUP BY status")
        .context("prepare count_by_status")?;
    let mut received = 0_i64;
    let mut succeeded = 0_i64;
    let mut failed = 0_i64;
    let iter = stmt
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))
        .context("query_map count_by_status")?;
    for r in iter {
        let (status, count) = r.context("read count_by_status row")?;
        match status.as_str() {
            "received" => received = count,
            "succeeded" => succeeded = count,
            "failed" => failed = count,
            other => return Err(eyre!("unexpected status value in receipts: {other}")),
        }
    }
    Ok((received, succeeded, failed))
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

/// Format `now - hours` as a [`TIMESTAMP_FMT`] lower bound for the
/// `*_since` counters.
pub fn hours_ago_iso(hours: i64) -> String {
    (Utc::now() - chrono::Duration::hours(hours))
        .format(TIMESTAMP_FMT)
        .to_string()
}

#[cfg(test)]
mod tests;
