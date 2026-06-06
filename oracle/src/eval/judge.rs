//! The relevance judge: grades how well a note answers a query.
//!
//! Injected via the [`RelevanceJudge`] trait so the eval pipeline runs against a
//! deterministic [`MockJudge`] in tests and the LLM-backed `FabricJudge` (added
//! in Phase 4) in production. The contract is strict for anti-circularity: a
//! judge receives ONLY the query plus the note's title and text — never the
//! search mode, score, tags, embeddings, or graph edges.

use std::collections::HashMap;

use eyre::Result;

/// Maximum graded relevance score. `0` = irrelevant, `MAX_SCORE` = perfect.
pub const MAX_SCORE: u8 = 3;
/// Score threshold at/above which a note counts as a relevant "hit" for
/// Recall and MRR (`2` = "good", `3` = "perfect"; `1` = marginal is not a hit).
pub const HIT_THRESHOLD: u8 = 2;

/// Grades the relevance of a note to a query on a `0..=MAX_SCORE` scale.
pub trait RelevanceJudge {
    /// Returns the graded relevance of `note_text` (a note titled `note_title`)
    /// to `query`. Implementations must clamp to `0..=MAX_SCORE`.
    fn judge(&self, query: &str, note_title: &str, note_text: &str) -> Result<u8>;
}

/// Deterministic, fixture-driven judge for tests (and dry runs). Looks up a
/// score by `(query, note_title)`; unknown pairs return `default`.
#[derive(Debug, Clone, Default)]
pub struct MockJudge {
    scores: HashMap<(String, String), u8>,
    default: u8,
}

impl MockJudge {
    /// A judge that returns `default` for every pair unless overridden.
    pub fn new(default: u8) -> Self {
        Self {
            scores: HashMap::new(),
            default: default.min(MAX_SCORE),
        }
    }

    /// Override the score for a specific `(query, title)` pair (builder style).
    pub fn with(mut self, query: &str, title: &str, score: u8) -> Self {
        self.scores
            .insert((query.to_string(), title.to_string()), score.min(MAX_SCORE));
        self
    }
}

impl RelevanceJudge for MockJudge {
    fn judge(&self, query: &str, note_title: &str, _note_text: &str) -> Result<u8> {
        let score = self
            .scores
            .get(&(query.to_string(), note_title.to_string()))
            .copied()
            .unwrap_or(self.default);
        Ok(score.min(MAX_SCORE))
    }
}

#[cfg(test)]
mod tests;
