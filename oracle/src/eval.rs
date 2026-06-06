//! `sb oracle eval` — relevance-lift measurement harness.
//!
//! Measures whether graph-augmented retrieval (`graph` / `graph-hybrid`) beats
//! the `hybrid` baseline, using a pooled, blind LLM-judge calibrated against
//! hand labels. Design: `docs/design/2026-06-06-oracle-eval-relevance-lift.md`.
//!
//! Library-only: this module returns typed data; `sb` renders it. The judge is
//! injected via the [`judge::RelevanceJudge`] trait so tests run without an LLM.

pub mod cache;
pub mod judge;
pub mod metrics;
pub mod queries;

use std::path::PathBuf;

use eyre::Result;

use crate::config::Config;

pub use judge::{MockJudge, RelevanceJudge};
pub use queries::{EvalQuery, Queries};

/// CLI-derived options for an eval run.
#[derive(Debug, Clone)]
pub struct EvalOpts {
    /// Path to the query set (`config/eval/queries.yml`).
    pub queries_path: PathBuf,
    /// Pool and metric depth `K` (e.g. nDCG@K).
    pub k: u32,
    /// Judge model name; empty string means "fabric's default model".
    pub judge_model: String,
    /// Ignore and overwrite cached judgments.
    pub rebuild_cache: bool,
}

impl Default for EvalOpts {
    fn default() -> Self {
        Self {
            queries_path: PathBuf::from("config/eval/queries.yml"),
            k: 10,
            judge_model: String::new(),
            rebuild_cache: false,
        }
    }
}

/// Phase-1 summary of a loaded query set. Later phases return the full metrics
/// report; this proves the CLI + loader wiring end to end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalSummary {
    pub query_count: usize,
    pub calibration_count: usize,
}

/// Load and validate the query set. (Phases 2-5 extend this to run the modes,
/// judge the pool, and compute the metrics report.)
pub fn run(_config: &Config, opts: &EvalOpts) -> Result<EvalSummary> {
    tracing::debug!(queries_path = %opts.queries_path.display(), k = opts.k, "eval::run");
    let queries = Queries::load(&opts.queries_path)?;
    let summary = EvalSummary {
        query_count: queries.queries.len(),
        calibration_count: queries.calibration().count(),
    };
    tracing::debug!(
        query_count = summary.query_count,
        calibration_count = summary.calibration_count,
        "eval::run loaded query set"
    );
    Ok(summary)
}
