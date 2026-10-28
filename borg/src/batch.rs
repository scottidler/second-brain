//! One summary per `sb borg ingest` bulk (design:
//! `docs/design/2026-10-27-multi-target-ingest.md`).
//!
//! A bulk's URL items carry a client batch id. Intake creates the `batches`
//! row ([`create_if_absent`]) and stamps `receipts.batch_id`; the client seals
//! the batch with the trace ids it saw queued ([`seal`]). After every terminal
//! write of a batched trace, on seal, and on the watchdog's backstop tick, the
//! daemon asks [`maybe_summarize`] / [`backstop`]: when the batch has drained,
//! exactly one caller wins the claim and gets a [`BatchSummary`] to put on the
//! notifier channel ([`notifier`]). Nothing here touches a sink, so sink
//! latency never sits inside a receipts write.

use chrono::{DateTime, Utc};
use eyre::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::receipts::TIMESTAMP_FMT;

pub mod notifier;

/// The daemon-side end of the summary channel: the `/ingest` task, the seal
/// route, and the watchdog each hold one; the notifier task holds the receiver.
pub type SummarySender = tokio::sync::mpsc::UnboundedSender<BatchSummary>;

/// Longest batch id the door accepts. A client uuid is 36 characters.
const MAX_ID_LEN: usize = 64;

/// `POST /batch/{id}/seal` body: the trace ids the client saw `Queued`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SealRequest {
    pub trace_ids: Vec<String>,
}

/// What [`seal`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SealOutcome {
    /// First seal: the trace ids are recorded.
    Sealed,
    /// Already sealed with this exact id set: a retried seal, a no-op.
    AlreadySealed,
    /// No member ever reached intake with this batch id.
    UnknownBatch,
    /// Already sealed with a different id set; the first seal stands.
    Conflict,
}

/// The one message a drained batch produces. Counts are over every receipts
/// row carrying the batch id (the daemon's membership), not the client's view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchSummary {
    pub batch_id: String,
    pub succeeded: u64,
    /// Of `succeeded`, published from a distill fallback.
    pub degraded: u64,
    /// `failed` rows, plus `rejected` ones (harvest-only, never expected here).
    pub failed: u64,
    /// Of `failed`, the watchdog's `crashed` promotions.
    pub crashed: u64,
    pub total: u64,
    /// Sealed trace ids with no receipts row in this batch. Should be 0.
    pub unknown: u64,
    /// The watchdog backstop summarized a batch the client never sealed.
    pub seal_missing: bool,
}

impl BatchSummary {
    /// `borg bulk <id>: <s> succeeded (<d> degraded), <f> failed (<c> crashed)
    /// of <total> URL items`, plus `, seal missing` / `, <u> unknown` when set.
    pub fn text(&self) -> String {
        let mut text = format!(
            "borg bulk {}: {} succeeded ({} degraded), {} failed ({} crashed) of {} URL items",
            self.batch_id, self.succeeded, self.degraded, self.failed, self.crashed, self.total
        );
        if self.seal_missing {
            text.push_str(", seal missing");
        }
        if self.unknown > 0 {
            text.push_str(&format!(", {} unknown", self.unknown));
        }
        text
    }
}

/// A batch id the door accepts: 1..=64 ASCII alphanumerics or `-`. The client
/// sends a uuid; anything else is refused before intake.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_LEN && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn now_iso() -> String {
    Utc::now().format(TIMESTAMP_FMT).to_string()
}

/// Create the `batches` row for `batch_id` unless it exists. Every member's
/// intake calls this before its receipts INSERT, so concurrent intakes of one
/// batch race here harmlessly.
pub fn create_if_absent(conn: &Connection, batch_id: &str) -> Result<()> {
    log::debug!("batch::create_if_absent: batch={batch_id}");
    conn.execute(
        "INSERT OR IGNORE INTO batches (id, created_at) VALUES (?, ?)",
        params![batch_id, now_iso()],
    )
    .with_context(|| format!("Failed to create batch row {batch_id}"))?;
    Ok(())
}

/// The batch a trace belongs to, or `None` for an unbatched (or unknown) trace.
pub fn batch_of(conn: &Connection, trace_id: &str) -> Result<Option<String>> {
    let batch: Option<Option<String>> = conn
        .query_row(
            "SELECT batch_id FROM receipts WHERE trace_id=?",
            params![trace_id],
            |row| row.get(0),
        )
        .optional()
        .with_context(|| format!("Failed to read batch_id of trace {trace_id}"))?;
    Ok(batch.flatten())
}

/// How many distinct trace ids a seal records.
pub fn seal_count(trace_ids: &[String]) -> usize {
    sorted_unique(trace_ids).len()
}

fn sorted_unique(ids: &[String]) -> Vec<String> {
    let mut ids = ids.to_vec();
    ids.sort();
    ids.dedup();
    ids
}

fn read_sealed(conn: &Connection, batch_id: &str) -> Result<Option<Option<Vec<String>>>> {
    let row: Option<Option<String>> = conn
        .query_row(
            "SELECT sealed_trace_ids FROM batches WHERE id=?",
            params![batch_id],
            |row| row.get(0),
        )
        .optional()
        .with_context(|| format!("Failed to read batch {batch_id}"))?;
    match row {
        None => Ok(None),
        Some(None) => Ok(Some(None)),
        Some(Some(json)) => {
            let ids: Vec<String> = serde_json::from_str(&json)
                .with_context(|| format!("batch {batch_id} has unparseable sealed_trace_ids {json:?}"))?;
            Ok(Some(Some(ids)))
        }
    }
}

/// Record the client's seal: the trace ids it saw `Queued`. The first seal
/// wins; an identical re-seal is [`SealOutcome::AlreadySealed`], a different
/// set is [`SealOutcome::Conflict`]. Ids compare as a set.
pub fn seal(conn: &Connection, batch_id: &str, trace_ids: &[String]) -> Result<SealOutcome> {
    let ids = sorted_unique(trace_ids);
    log::debug!("batch::seal: batch={batch_id} trace_ids={}", ids.len());
    let json = serde_json::to_string(&ids).context("serialize sealed trace ids")?;
    // Two attempts: a concurrent seal can land between the read and the
    // guarded UPDATE, and the second read then judges against it.
    for _ in 0..2 {
        match read_sealed(conn, batch_id)? {
            None => return Ok(SealOutcome::UnknownBatch),
            Some(Some(existing)) => {
                return Ok(if sorted_unique(&existing) == ids {
                    SealOutcome::AlreadySealed
                } else {
                    SealOutcome::Conflict
                });
            }
            Some(None) => {
                let rows = conn
                    .execute(
                        "UPDATE batches SET sealed_trace_ids=?, sealed_at=? WHERE id=? AND sealed_at IS NULL",
                        params![json, now_iso(), batch_id],
                    )
                    .with_context(|| format!("Failed to seal batch {batch_id}"))?;
                if rows == 1 {
                    log::info!("batch {batch_id}: sealed with {} trace id(s)", ids.len());
                    return Ok(SealOutcome::Sealed);
                }
            }
        }
    }
    eyre::bail!("batch {batch_id}: seal kept racing a concurrent seal")
}

/// Drained = sealed, no member still `received`, and every sealed id has a
/// receipts row. The first half waits for a member whose POST response was
/// lost (the client never counted it); the second waits for a member the
/// client saw acknowledged.
pub fn drained(conn: &Connection, batch_id: &str) -> Result<bool> {
    let Some(Some(sealed)) = read_sealed(conn, batch_id)? else {
        return Ok(false);
    };
    let in_flight: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM receipts WHERE batch_id=? AND status='received'",
            params![batch_id],
            |row| row.get(0),
        )
        .with_context(|| format!("Failed to count in-flight members of batch {batch_id}"))?;
    if in_flight > 0 {
        log::debug!("batch::drained: batch={batch_id} in_flight={in_flight}");
        return Ok(false);
    }
    for trace_id in &sealed {
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM receipts WHERE trace_id=?)",
                params![trace_id],
                |row| row.get(0),
            )
            .with_context(|| format!("Failed to probe sealed trace {trace_id}"))?;
        if !exists {
            log::debug!("batch::drained: batch={batch_id} sealed trace {trace_id} has no row yet");
            return Ok(false);
        }
    }
    Ok(true)
}

/// Count the batch's member rows into a summary.
fn count(conn: &Connection, batch_id: &str, seal_missing: bool) -> Result<BatchSummary> {
    let (total, succeeded, degraded, failed, crashed): (i64, i64, i64, i64, i64) = conn
        .query_row(
            "SELECT COUNT(*), \
               COALESCE(SUM(status='succeeded'), 0), \
               COALESCE(SUM(status='succeeded' AND degraded=1), 0), \
               COALESCE(SUM(status IN ('failed', 'rejected')), 0), \
               COALESCE(SUM(status='failed' AND failure_stage='crashed'), 0) \
             FROM receipts WHERE batch_id=?",
            params![batch_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .with_context(|| format!("Failed to count members of batch {batch_id}"))?;
    let sealed = read_sealed(conn, batch_id)?.flatten().unwrap_or_default();
    let mut unknown = 0u64;
    for trace_id in &sealed {
        let member: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM receipts WHERE trace_id=? AND batch_id=?)",
                params![trace_id, batch_id],
                |row| row.get(0),
            )
            .with_context(|| format!("Failed to probe sealed trace {trace_id}"))?;
        if !member {
            unknown += 1;
        }
    }
    Ok(BatchSummary {
        batch_id: batch_id.to_string(),
        succeeded: succeeded as u64,
        degraded: degraded as u64,
        failed: failed as u64,
        crashed: crashed as u64,
        total: total as u64,
        unknown,
        seal_missing,
    })
}

/// Count, then claim the one summary with the conditional UPDATE. Only the
/// caller whose UPDATE changed one row gets `Some`. Counting first means a
/// failed count leaves the claim for the next trigger instead of losing it.
fn claim(conn: &Connection, batch_id: &str, seal_missing: bool) -> Result<Option<BatchSummary>> {
    let summary = count(conn, batch_id, seal_missing)?;
    // The backstop claim also requires the batch to still be unsealed, so a
    // seal landing between its SELECT and this UPDATE is not summarized as
    // "seal missing".
    let sql = if seal_missing {
        "UPDATE batches SET summarized_at=? WHERE id=? AND summarized_at IS NULL AND sealed_at IS NULL"
    } else {
        "UPDATE batches SET summarized_at=? WHERE id=? AND summarized_at IS NULL"
    };
    let rows = conn
        .execute(sql, params![now_iso(), batch_id])
        .with_context(|| format!("Failed to claim the summary of batch {batch_id}"))?;
    if rows == 1 {
        log::info!("batch {batch_id}: summary claimed ({})", summary.text());
        Ok(Some(summary))
    } else {
        log::debug!("batch::claim: batch={batch_id} already summarized (or sealed, for the backstop)");
        Ok(None)
    }
}

/// The drain check: `Some(summary)` for exactly one caller once the batch has
/// drained, `None` for every other call. Sync, and never touches a sink.
pub fn maybe_summarize(conn: &Connection, batch_id: &str) -> Result<Option<BatchSummary>> {
    log::debug!("batch::maybe_summarize: batch={batch_id}");
    if !drained(conn, batch_id)? {
        return Ok(None);
    }
    claim(conn, batch_id, false)
}

/// [`maybe_summarize`] for the batch `trace_id` belongs to, after its terminal
/// write. An unbatched trace is `None` with one indexed read.
pub fn after_terminal(conn: &Connection, trace_id: &str) -> Result<Option<BatchSummary>> {
    match batch_of(conn, trace_id)? {
        Some(batch_id) => maybe_summarize(conn, &batch_id),
        None => Ok(None),
    }
}

/// The watchdog's seal-missing backstop: every batch that is unsealed and
/// unsummarized, has no member still `received`, and whose newest member was
/// received more than `deadline_secs` before `now`, is claimed and summarized
/// with `seal_missing`. A sealed batch is never touched here: its own drain
/// check owns it.
pub fn backstop(conn: &Connection, deadline_secs: u64, now: DateTime<Utc>) -> Result<Vec<BatchSummary>> {
    let cutoff = (now - chrono::Duration::seconds(deadline_secs as i64))
        .format(TIMESTAMP_FMT)
        .to_string();
    let mut stmt = conn
        .prepare(
            "SELECT b.id FROM batches b \
             WHERE b.sealed_at IS NULL AND b.summarized_at IS NULL \
             AND EXISTS (SELECT 1 FROM receipts r WHERE r.batch_id = b.id) \
             AND NOT EXISTS (SELECT 1 FROM receipts r WHERE r.batch_id = b.id AND r.status = 'received') \
             AND (SELECT MAX(r.received_at) FROM receipts r WHERE r.batch_id = b.id) < ?",
        )
        .context("prepare batch backstop")?;
    let ids = stmt
        .query_map(params![cutoff], |row| row.get::<_, String>(0))
        .context("query batch backstop")?
        .collect::<rusqlite::Result<Vec<String>>>()
        .context("read batch backstop rows")?;
    let mut summaries = Vec::new();
    for batch_id in ids {
        log::warn!("batch {batch_id}: never sealed and idle past {deadline_secs}s; summarizing as seal missing");
        if let Some(summary) = claim(conn, &batch_id, true)? {
            summaries.push(summary);
        }
    }
    Ok(summaries)
}

/// Put a claimed summary on the notifier channel. A closed channel (the
/// notifier task is gone) loses this one best-effort summary, loudly.
pub fn send(tx: &SummarySender, summary: BatchSummary) {
    let batch_id = summary.batch_id.clone();
    if tx.send(summary).is_err() {
        log::error!("batch {batch_id}: notifier channel closed; summary dropped");
    }
}

#[cfg(test)]
mod tests;
