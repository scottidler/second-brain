//! Ingest-queue snapshot: "is borg still chewing on what I just sent it?".
//!
//! [`snapshot`] is the one place the batch and the per-item partition are
//! computed; it is pure over injected rows and `now`. [`load`] is the SQL read
//! that feeds it. The wire shape is `vault::queue::QueueSnapshot`, shared with
//! every client. Design: `docs/design/2026-10-04-ingest-queue-status.md`.
//!
//! Batch rule: every candidate row is an interval `[received_at, terminal_at
//! or now]`; sorted by `received_at`, intervals whose gap is at most
//! `queue.batch-gap` merge into one batch, identified by the trace id of its
//! earliest-received member. Plain `/queue` reports the batch holding the
//! newest in-flight row (or plain idle); `?batch=<id>` reports that batch
//! whether in flight or finished.

use std::str::FromStr;

use chrono::{DateTime, TimeDelta, Utc};
use eyre::{Context, Result, eyre};
use rusqlite::{Connection, params};
use vault::daemon::client::{DaemonClient, DaemonError};
use vault::queue::{BatchSummary, ItemState, QueueItem, QueueSnapshot, QueueState};
use vault::receipts::{FailureStage, ReceiptStatus};
use vault::schema::Method;

use crate::config::QueueConfig;
use crate::receipts::TIMESTAMP_FMT;

/// Character cap on an item's `source` and `failure-reason`.
pub const FIELD_MAX_CHARS: usize = 200;

/// How far back finished rows are considered. In-flight rows are visible at
/// any age; this bound only trims finished history.
pub const HISTORY_WINDOW_HOURS: i64 = 24;

/// Lifecycle status of a row that can belong to a batch. `rejected` is the
/// harvest selection gate's status and harvest is excluded from every batch,
/// so it has no variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStatus {
    Received,
    Succeeded,
    Failed,
}

/// One receipts row as the snapshot sees it, timestamps already parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueRow {
    pub trace_id: String,
    pub received_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub terminal_at: Option<DateTime<Utc>>,
    pub status: RowStatus,
    pub raw_input: String,
    pub failure_stage: Option<FailureStage>,
    pub failure_reason: Option<String>,
}

impl QueueRow {
    fn in_flight(&self) -> bool {
        self.status == RowStatus::Received
    }

    /// End of this row's interval: `now` while in flight, else its terminal
    /// time (a terminal row missing `terminal_at` collapses to its receipt).
    fn interval_end(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        if self.in_flight() {
            now
        } else {
            self.terminal_at.unwrap_or(self.received_at)
        }
    }
}

/// Every row's state; `Succeeded` is counted but never listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Partition {
    Queued,
    Processing,
    Wedged,
    Succeeded,
    Failed,
}

impl Partition {
    fn item_state(self) -> Option<ItemState> {
        match self {
            Self::Queued => Some(ItemState::Queued),
            Self::Processing => Some(ItemState::Processing),
            Self::Wedged => Some(ItemState::Wedged),
            Self::Failed => Some(ItemState::Failed),
            Self::Succeeded => None,
        }
    }
}

fn whole_secs(delta: TimeDelta) -> u64 {
    delta.num_seconds().max(0) as u64
}

/// A std `Duration` from config as a chrono delta. A value too large for
/// chrono saturates, which keeps the comparison it feeds meaningful.
fn to_delta(d: std::time::Duration) -> TimeDelta {
    TimeDelta::from_std(d).unwrap_or(TimeDelta::MAX)
}

/// In-flight age: `now - COALESCE(started_at, received_at)`. Queued rows age
/// from receipt so a NULL-`started_at` row (missed stamp, restart before
/// grant) still reaches wedged.
fn age(row: &QueueRow, now: DateTime<Utc>) -> TimeDelta {
    now - row.started_at.unwrap_or(row.received_at)
}

fn partition(row: &QueueRow, now: DateTime<Utc>, wedged_after: TimeDelta) -> Partition {
    match row.status {
        RowStatus::Succeeded => Partition::Succeeded,
        RowStatus::Failed => Partition::Failed,
        RowStatus::Received if age(row, now) > wedged_after => Partition::Wedged,
        RowStatus::Received if row.started_at.is_some() => Partition::Processing,
        RowStatus::Received => Partition::Queued,
    }
}

/// Merge rows into batches. Input order is irrelevant; output batches and
/// their members are in `received_at` order (ties by trace id).
fn batches(rows: &[QueueRow], now: DateTime<Utc>, gap: TimeDelta) -> Vec<Vec<&QueueRow>> {
    let mut sorted: Vec<&QueueRow> = rows.iter().collect();
    sorted.sort_by(|a, b| {
        a.received_at
            .cmp(&b.received_at)
            .then_with(|| a.trace_id.cmp(&b.trace_id))
    });
    let mut groups: Vec<Vec<&QueueRow>> = Vec::new();
    let mut group_end: Option<DateTime<Utc>> = None;
    for row in sorted {
        let end = row.interval_end(now);
        match (groups.last_mut(), group_end) {
            (Some(group), Some(current_end)) if row.received_at - current_end <= gap => {
                log::trace!(
                    "queue::batches: trace={} joins batch={}",
                    row.trace_id,
                    group[0].trace_id
                );
                group.push(row);
                group_end = Some(current_end.max(end));
            }
            _ => {
                log::trace!("queue::batches: trace={} opens a new batch", row.trace_id);
                groups.push(vec![row]);
                group_end = Some(end);
            }
        }
    }
    groups
}

fn item(row: &QueueRow, state: ItemState, now: DateTime<Utc>) -> QueueItem {
    let source = vault::text::truncate(&row.raw_input, FIELD_MAX_CHARS).to_string();
    if state == ItemState::Failed {
        QueueItem {
            trace: row.trace_id.clone(),
            state,
            source,
            received: None,
            started: None,
            age_secs: None,
            failure_stage: row.failure_stage,
            failure_reason: row
                .failure_reason
                .as_deref()
                .map(|r| vault::text::truncate(r, FIELD_MAX_CHARS).to_string()),
        }
    } else {
        QueueItem {
            trace: row.trace_id.clone(),
            state,
            source,
            received: Some(row.received_at.format(TIMESTAMP_FMT).to_string()),
            started: row.started_at.map(|s| s.format(TIMESTAMP_FMT).to_string()),
            age_secs: Some(whole_secs(age(row, now))),
            failure_stage: None,
            failure_reason: None,
        }
    }
}

/// Summarize one batch (non-empty, `received_at`-ordered members).
fn summarize(group: &[&QueueRow], now: DateTime<Utc>, wedged_after: TimeDelta) -> QueueSnapshot {
    let first = group[0];
    let (mut succeeded, mut failed, mut queued, mut processing, mut wedged) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut items = Vec::new();
    let mut last_end = first.received_at;
    for row in group {
        let p = partition(row, now, wedged_after);
        match p {
            Partition::Queued => queued += 1,
            Partition::Processing => processing += 1,
            Partition::Wedged => wedged += 1,
            Partition::Succeeded => succeeded += 1,
            Partition::Failed => failed += 1,
        }
        if let Some(state) = p.item_state() {
            items.push(item(row, state, now));
        }
        last_end = last_end.max(row.interval_end(now));
    }
    let remaining = queued + processing + wedged;
    let state = if remaining > 0 { QueueState::Draining } else { QueueState::Idle };
    // Draining: any in-flight row's interval ends at `now`, so `last_end`
    // is `now`; idle: it is the last terminal time. One expression for both.
    let elapsed_secs = whole_secs(last_end - first.received_at);
    QueueSnapshot {
        state,
        batch: Some(BatchSummary {
            id: first.trace_id.clone(),
            started: first.received_at.format(TIMESTAMP_FMT).to_string(),
            elapsed_secs,
            total: group.len() as u64,
            done: succeeded + failed,
            remaining,
            succeeded,
            failed,
            queued,
            processing,
            wedged,
        }),
        items: Some(items),
    }
}

/// Compute the queue snapshot from candidate `rows` (already filtered to
/// non-harvest, in flight or finished within the history window).
///
/// - `batch == None`: the batch holding the newest in-flight row, or plain
///   idle (`{"state":"idle"}`) when nothing is in flight. Always `Some`.
/// - `batch == Some(id)`: the batch containing trace `id` (normally its
///   earliest member, the id `wait` pinned), draining or idle, `batch` +
///   `items` always present. `None` when no batch contains `id` (aged out of
///   the window or never existed); the route maps it to 404.
pub fn snapshot(
    rows: &[QueueRow],
    now: DateTime<Utc>,
    cfg: &QueueConfig,
    batch: Option<&str>,
) -> Option<QueueSnapshot> {
    log::debug!(
        "queue::snapshot: rows={} now={} wedged_after={:?} batch_gap={:?} batch={:?}",
        rows.len(),
        now.format(TIMESTAMP_FMT),
        cfg.wedged_after,
        cfg.batch_gap,
        batch
    );
    let wedged_after = to_delta(cfg.wedged_after);
    let groups = batches(rows, now, to_delta(cfg.batch_gap));
    let chosen = match batch {
        // Membership, not first-member: a later arrival in the same second
        // can sort ahead of the pinned id (1s timestamps, ties by trace id),
        // and `wait` must keep finding its batch when that happens.
        Some(id) => match groups.iter().find(|g| g.iter().any(|r| r.trace_id == id)) {
            Some(group) => group,
            None => {
                log::debug!("queue::snapshot: batch={id} not found among {} batches", groups.len());
                return None;
            }
        },
        None => {
            let newest_in_flight = groups
                .iter()
                .filter_map(|g| {
                    g.iter()
                        .filter(|r| r.in_flight())
                        .map(|r| (r.received_at, g))
                        .max_by_key(|(t, _)| *t)
                })
                .max_by_key(|(t, _)| *t);
            match newest_in_flight {
                Some((_, group)) => group,
                None => {
                    log::debug!(
                        "queue::snapshot: nothing in flight across {} batches -> idle",
                        groups.len()
                    );
                    return Some(QueueSnapshot::idle());
                }
            }
        }
    };
    let snap = summarize(chosen, now, wedged_after);
    if let Some(b) = &snap.batch {
        log::debug!(
            "queue::snapshot: state={:?} batch={} total={} done={} remaining={} wedged={}",
            snap.state,
            b.id,
            b.total,
            b.done,
            b.remaining,
            b.wedged
        );
    }
    Some(snap)
}

fn parse_ts(trace_id: &str, column: &str, raw: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|t| t.with_timezone(&Utc))
        .with_context(|| format!("receipts row {trace_id}: unparseable {column} {raw:?}"))
}

fn parse_opt_ts(trace_id: &str, column: &str, raw: Option<String>) -> Result<Option<DateTime<Utc>>> {
    raw.map(|r| parse_ts(trace_id, column, &r)).transpose()
}

/// Raw column values as SELECTed, before parsing.
struct RawRow {
    trace_id: String,
    received_at: String,
    started_at: Option<String>,
    terminal_at: Option<String>,
    status: String,
    raw_input: String,
    failure_stage: Option<String>,
    failure_reason: Option<String>,
}

/// Parse one row. Unparseable timestamps, statuses or stages are errors, never
/// silently dropped rows: a dropped in-flight row would read as idle.
fn parse_row(raw: RawRow) -> Result<QueueRow> {
    let status = match ReceiptStatus::from_str(&raw.status).map_err(|e| eyre!("receipts row {}: {e}", raw.trace_id))? {
        ReceiptStatus::Received => RowStatus::Received,
        ReceiptStatus::Succeeded => RowStatus::Succeeded,
        ReceiptStatus::Failed => RowStatus::Failed,
        ReceiptStatus::Rejected => {
            return Err(eyre!(
                "receipts row {}: rejected rows are excluded by the query",
                raw.trace_id
            ));
        }
    };
    let failure_stage = raw
        .failure_stage
        .as_deref()
        .map(FailureStage::from_str)
        .transpose()
        .map_err(|e| eyre!("receipts row {}: {e}", raw.trace_id))?;
    Ok(QueueRow {
        received_at: parse_ts(&raw.trace_id, "received_at", &raw.received_at)?,
        started_at: parse_opt_ts(&raw.trace_id, "started_at", raw.started_at)?,
        terminal_at: parse_opt_ts(&raw.trace_id, "terminal_at", raw.terminal_at)?,
        status,
        raw_input: raw.raw_input,
        failure_stage,
        failure_reason: raw.failure_reason,
        trace_id: raw.trace_id,
    })
}

/// Read candidate rows from the receipts DB and compute [`snapshot`].
///
/// Candidates: `method != harvest AND status != rejected AND (status =
/// 'received' OR received_at >= now - 24h)`. In-flight rows are visible at any
/// age; the window only trims finished history. A DB or parse error is an
/// error, never an idle snapshot.
pub fn load(
    conn: &Connection,
    now: DateTime<Utc>,
    cfg: &QueueConfig,
    batch: Option<&str>,
) -> Result<Option<QueueSnapshot>> {
    let cutoff = (now - TimeDelta::hours(HISTORY_WINDOW_HOURS))
        .format(TIMESTAMP_FMT)
        .to_string();
    log::debug!(
        "queue::load: now={} cutoff={} batch={:?}",
        now.format(TIMESTAMP_FMT),
        cutoff,
        batch
    );
    let mut stmt = conn
        .prepare(
            "SELECT trace_id, received_at, started_at, terminal_at, status, raw_input, \
                    failure_stage, failure_reason \
             FROM receipts \
             WHERE method != ?1 AND status != ?2 AND (status = ?3 OR received_at >= ?4)",
        )
        .context("prepare queue::load")?;
    let iter = stmt
        .query_map(
            params![
                Method::Harvest.as_str(),
                ReceiptStatus::Rejected.as_str(),
                ReceiptStatus::Received.as_str(),
                cutoff
            ],
            |row| {
                Ok(RawRow {
                    trace_id: row.get(0)?,
                    received_at: row.get(1)?,
                    started_at: row.get(2)?,
                    terminal_at: row.get(3)?,
                    status: row.get(4)?,
                    raw_input: row.get(5)?,
                    failure_stage: row.get(6)?,
                    failure_reason: row.get(7)?,
                })
            },
        )
        .context("query queue::load")?;
    let mut rows = Vec::new();
    for raw in iter {
        rows.push(parse_row(raw.context("read queue::load row")?)?);
    }
    log::debug!("queue::load: candidate_rows={}", rows.len());
    Ok(snapshot(&rows, now, cfg, batch))
}

/// Why [`fetch`] could not return a snapshot. Typed so callers (`sb borg wait`)
/// can tell an unknown batch from an old daemon from an unreachable one.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("daemon at {addr} predates /queue; run otto deploy there")]
    PredatesQueue { addr: String },
    #[error("unknown batch {batch}")]
    UnknownBatch { batch: String },
    #[error("hotkey.host/hotkey.port form no valid address ({addr}): {reason}")]
    BadAddress { addr: String, reason: String },
    #[error("daemon at {addr} refused the credentials (HTTP 401); check server.auth-token")]
    Unauthorized { addr: String },
    #[error("cannot build an HTTP client for the daemon at {addr}: {reason}")]
    Client { addr: String, reason: String },
    #[error("cannot reach daemon at {addr} for GET {path}: {source}")]
    Unreachable {
        addr: String,
        path: String,
        source: reqwest::Error,
    },
    #[error("daemon at {addr} answered GET {path} with HTTP {status}: {body}")]
    Http {
        addr: String,
        path: String,
        status: u16,
        body: String,
    },
    #[error("daemon at {addr} sent an unparseable /queue body: {source}")]
    Parse { addr: String, source: reqwest::Error },
}

/// `GET /queue[?batch=<id>]` on the daemon at `hotkey.host:hotkey.port`.
///
/// `timeout` bounds the whole request (connect, headers, body); callers own the
/// overall deadline. A 404 on plain `/queue` means an old daemon; a 404 with
/// `?batch=` means that batch is unknown.
pub async fn fetch(
    config: &crate::config::Config,
    batch: Option<&str>,
    timeout: std::time::Duration,
) -> Result<QueueSnapshot, FetchError> {
    let addr = format!("{}:{}", config.hotkey.host, config.hotkey.port);
    let path = match batch {
        Some(_) => "/queue?batch=<id>".to_string(),
        None => "/queue".to_string(),
    };
    log::debug!("queue::fetch: addr={addr} batch={batch:?} timeout={timeout:?}");
    let result = fetch_inner(config, &addr, &path, batch, timeout).await;
    match &result {
        Ok(snap) => log::debug!("queue::fetch: addr={addr} ok state={:?}", snap.state),
        Err(e) => log::warn!("queue::fetch: addr={addr} batch={batch:?} failed: {e}"),
    }
    result
}

async fn fetch_inner(
    config: &crate::config::Config,
    addr: &str,
    path: &str,
    batch: Option<&str>,
    timeout: std::time::Duration,
) -> Result<QueueSnapshot, FetchError> {
    let daemon = DaemonClient::new(&config.hotkey, config.server.auth_token.as_deref()).map_err(|e| match e {
        DaemonError::BadAddress { addr, reason } => FetchError::BadAddress { addr, reason },
        other => FetchError::Client {
            addr: addr.to_string(),
            reason: other.to_string(),
        },
    })?;
    let query: Vec<(&str, &str)> = batch.map(|id| ("batch", id)).into_iter().collect();
    let resp = daemon.get("/queue", &query, Some(timeout)).await.map_err(|e| match e {
        DaemonError::BadAddress { addr, reason } => FetchError::BadAddress { addr, reason },
        DaemonError::Unreachable { source, .. } => FetchError::Unreachable {
            addr: addr.to_string(),
            path: path.to_string(),
            source,
        },
        other => FetchError::Client {
            addr: addr.to_string(),
            reason: other.to_string(),
        },
    })?;
    let status = resp.status();
    if status.is_success() {
        return resp.json::<QueueSnapshot>().await.map_err(|source| FetchError::Parse {
            addr: addr.to_string(),
            source,
        });
    }
    match (status.as_u16(), batch) {
        (404, Some(id)) => Err(FetchError::UnknownBatch { batch: id.to_string() }),
        (404, None) => Err(FetchError::PredatesQueue { addr: addr.to_string() }),
        (401, _) => Err(FetchError::Unauthorized { addr: addr.to_string() }),
        (code, _) => {
            let body = resp.text().await.map_err(|source| FetchError::Unreachable {
                addr: addr.to_string(),
                path: path.to_string(),
                source,
            })?;
            Err(FetchError::Http {
                addr: addr.to_string(),
                path: path.to_string(),
                status: code,
                body: vault::text::truncate(&body, FIELD_MAX_CHARS).to_string(),
            })
        }
    }
}

#[cfg(test)]
mod tests;
