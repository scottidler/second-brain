//! The relevance judge: grades how well a note answers a query.
//!
//! Injected via the [`RelevanceJudge`] trait so the eval pipeline runs against a
//! deterministic [`MockJudge`] in tests and the LLM-backed `FabricJudge` (added
//! in Phase 4) in production. The contract is strict for anti-circularity: a
//! judge receives ONLY the query plus the note's title and text — never the
//! search mode, score, tags, embeddings, or graph edges.

use std::collections::HashMap;

use eyre::{Result, bail};

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

/// Production judge: runs the `judge-relevance` Fabric pattern over the
/// `(query, note)` text and parses a single integer score. The pattern is blind
/// by construction — it only ever sees the strings passed to [`judge`].
#[derive(Debug, Clone)]
pub struct FabricJudge {
    /// Fabric binary name (resolved on `PATH`).
    pub binary: String,
    /// Model name; empty = fabric's default model.
    pub model: String,
    /// Fabric pattern name (resolved under `~/.config/sb/patterns/`).
    pub pattern: String,
    /// Truncation budget (chars) for the note text sent to the judge.
    pub max_chars: usize,
    /// Per-call fabric timeout.
    pub timeout_secs: u64,
}

/// Default char budget for the judged note text.
const DEFAULT_JUDGE_MAX_CHARS: usize = 8_000;
/// Default per-call fabric timeout for the judge.
const DEFAULT_JUDGE_TIMEOUT_SECS: u64 = 60;

impl FabricJudge {
    /// A judge using the `judge-relevance` pattern and the given model
    /// (empty string = fabric's default model).
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            binary: "fabric".to_string(),
            model: model.into(),
            pattern: "judge-relevance".to_string(),
            max_chars: DEFAULT_JUDGE_MAX_CHARS,
            timeout_secs: DEFAULT_JUDGE_TIMEOUT_SECS,
        }
    }
}

impl RelevanceJudge for FabricJudge {
    fn judge(&self, query: &str, note_title: &str, note_text: &str) -> Result<u8> {
        let input = format!("# QUERY\n{query}\n\n# NOTE TITLE\n{note_title}\n\n# NOTE\n{note_text}\n");
        let reply = vault::fabric::run_pattern(
            &self.pattern,
            &input,
            &self.binary,
            &self.model,
            self.max_chars,
            self.timeout_secs,
        )?;
        parse_score(&reply)
    }
}

/// Parse a graded score from a judge reply: the first integer token, clamped to
/// `0..=MAX_SCORE`. Errors when the reply contains no integer (caller treats the
/// pair as uncovered rather than silently scoring 0).
pub fn parse_score(reply: &str) -> Result<u8> {
    for token in reply.split(|c: char| !c.is_ascii_digit()) {
        if let Ok(n) = token.parse::<u32>() {
            return Ok((n.min(MAX_SCORE as u32)) as u8);
        }
    }
    bail!(
        "judge reply contains no integer score: {:?}",
        reply.chars().take(80).collect::<String>()
    )
}

#[cfg(test)]
mod tests;
