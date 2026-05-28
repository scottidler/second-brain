//! Dreaming: non-destructive consolidation across the chunk corpus.
//!
//! Three detectors run on a slower cadence than tier 1/2:
//! - `dedup`: pairs of work-items that should be one
//! - `xref`:  cross-reference opportunities between work-items
//! - `stale`: chunks whose member sessions have grown since last distill
//!
//! Each detector writes proposals to `notes/glean-dreams/<kind>-<hash>.md`.
//! Dreams are never persisted in SQLite; the proposal file is the
//! only durable state. Re-running with no corpus change is a no-op
//! (content-addressed filenames).

pub mod dedup;
pub mod render;
pub mod stale;
pub mod xref;

use eyre::Result;

use crate::config::Config;
use crate::ledger::Ledger;

#[derive(Debug, Clone)]
pub struct DreamReport {
    pub n_dedup: usize,
    pub n_xref: usize,
    pub n_stale: usize,
}

/// Run all three detectors against the current corpus.
pub fn run_all(ledger: &Ledger, config: &Config) -> Result<DreamReport> {
    log::info!("dream::run_all");
    let n_dedup = dedup::run(ledger, config)?;
    let n_xref = xref::run(ledger, config)?;
    let n_stale = stale::run(ledger, config)?;
    Ok(DreamReport {
        n_dedup,
        n_xref,
        n_stale,
    })
}
