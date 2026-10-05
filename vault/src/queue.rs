//! Wire contract for the borg ingest-queue snapshot (`GET /queue`).
//!
//! Computed in ONE place, `borg::queue::snapshot` on the daemon host, and
//! rendered verbatim by every client (`sb borg queue`, `sb borg wait`, oracle's
//! `ingest_queue`). Shared here so the server and its clients cannot disagree
//! on the shape. Design: `docs/design/2026-10-04-ingest-queue-status.md`.

use serde::{Deserialize, Serialize};

use crate::receipts::FailureStage;

/// Whether the reported batch still has in-flight items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QueueState {
    Idle,
    Draining,
}

/// The state of one reported item. Succeeded items are counted in
/// [`BatchSummary::succeeded`] but never listed, so they have no variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ItemState {
    /// In flight, no general permit yet, younger than `queue.wedged-after`.
    Queued,
    /// In flight, permit granted, younger than `queue.wedged-after`.
    Processing,
    /// In flight longer than `queue.wedged-after` (aged from the permit grant,
    /// or from receipt when never granted). Not proof of a deadlock.
    Wedged,
    /// Terminal `failed` (includes watchdog `crashed`).
    Failed,
}

/// One ingest-queue answer. Plain idle is exactly `{"state":"idle"}`: `batch`
/// and `items` are skipped when absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct QueueSnapshot {
    pub state: QueueState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<BatchSummary>,
    /// Every in-flight or failed item in the batch; succeeded omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<QueueItem>>,
}

impl QueueSnapshot {
    /// Nothing in flight, no batch requested.
    pub fn idle() -> Self {
        Self {
            state: QueueState::Idle,
            batch: None,
            items: None,
        }
    }
}

/// Counts for one batch. `done` and `remaining` are sums of their parts,
/// computed alongside them so they cannot drift.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct BatchSummary {
    /// Trace id of the batch's earliest-received member.
    pub id: String,
    /// `received_at` of that member (`%Y-%m-%dT%H:%M:%SZ`).
    pub started: String,
    /// `now - started` while draining; `last terminal - started` when idle.
    pub elapsed_secs: u64,
    pub total: u64,
    /// `succeeded + failed`.
    pub done: u64,
    /// `queued + processing + wedged`.
    pub remaining: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub queued: u64,
    pub processing: u64,
    pub wedged: u64,
}

/// One in-flight or failed item. In-flight items carry `received`, `started`
/// (once the permit is granted) and `age-secs`; failed items carry
/// `failure-stage` and `failure-reason`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct QueueItem {
    pub trace: String,
    pub state: ItemState,
    /// The receipt's `raw_input`, truncated to 200 characters.
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub received: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_stage: Option<FailureStage>,
    /// Truncated to 200 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
}

#[cfg(test)]
mod tests;
