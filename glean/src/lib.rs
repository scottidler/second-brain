#![deny(clippy::unwrap_used)]
#![deny(dead_code)]
#![deny(unused_variables)]
#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

//! glean - distills Claude Code session transcripts into per-work-item
//! knowledge chunks.
//!
//! Two-tier pipeline plus a non-destructive consolidation pass.
//! Tier 1 (`harvest`): one JSONL session file becomes one
//! `SessionRecord` in the `sessions` table. Cluster: session-records
//! group into work-items via design-doc anchor (hard) and embedding
//! similarity (soft). Tier 2 (`distill`): each work-item becomes one
//! markdown chunk under `notes/glean/`. Dreaming (async): three
//! detectors (dedup, xref, stale) write proposals to
//! `notes/glean-dreams/` for operator review.

pub mod classify;
pub mod cluster;
pub mod config;
pub mod daemon;
pub mod distill;
pub mod dream;
pub mod error;
pub mod harvest;
pub mod jsonl;
pub mod ledger;
pub mod opts;
pub mod render;
pub mod repo;
pub mod scan;
pub mod types;

pub use config::Config;
pub use error::GleanError;
pub use harvest::{HarvestReport, run as harvest};
pub use ledger::Ledger;
pub use types::{QuarantineRecord, SessionRecord, WorkItem, WorkItemKey};
