//! `sb borg eval --scored --limit N`: the traces the score table most recently
//! scored, loaded from staging, so the `eval/` tools judge exactly the traces
//! `eval_scores` already holds a baseline for.
//!
//! A scored trace whose staged note changed since it was scored (re-distilled,
//! so its distill hash moved) is skipped and counted: a verdict for it would be
//! stale on arrival.

use std::path::Path;

use eyre::Result;

use super::scores::ScoreStore;
use super::{Fixture, RUN_KIND_BASELINE, judged_input, load_staged_trace};

/// The selection, and what was passed over.
#[derive(Debug, Default)]
pub struct ScoredSelection {
    pub fixtures: Vec<Fixture>,
    /// Scored traces no longer in staging.
    pub not_staged: usize,
    /// Scored traces whose staged note no longer hashes to a scored baseline.
    pub moved_on: Vec<String>,
}

/// The `limit` most recently scored traces that are still staged at the hash
/// they were scored at.
pub fn select_scored(store: &ScoreStore, staged_root: &Path, limit: usize) -> Result<ScoredSelection> {
    log::debug!(
        "eval::scored::select_scored: root={} limit={limit}",
        staged_root.display()
    );
    let mut selection = ScoredSelection::default();
    for trace in store.latest_scored_traces()? {
        if selection.fixtures.len() >= limit {
            break;
        }
        let Some(fixture) = load_staged_trace(staged_root, &trace)? else {
            selection.not_staged += 1;
            continue;
        };
        let hash = judged_input(&fixture).content_hash;
        if store.run_kind_of(&trace, RUN_KIND_BASELINE, "", &hash)?.is_none() {
            selection.moved_on.push(trace);
            continue;
        }
        selection.fixtures.push(fixture);
    }
    Ok(selection)
}

#[cfg(test)]
mod tests;
