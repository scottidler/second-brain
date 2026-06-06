//! The eval query set (`config/eval/queries.yml`) and its loader.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use eyre::{Context, Result, bail};
use serde::Deserialize;

/// The full query set loaded from `queries.yml`.
#[derive(Debug, Clone, Deserialize)]
pub struct Queries {
    pub queries: Vec<EvalQuery>,
}

/// One evaluation query. `calibration` is non-empty only on the handful of
/// queries used to validate the LLM judge against hand labels.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct EvalQuery {
    /// Stable identifier; also part of the judgment cache key. Never reuse an id
    /// for a different query (it would alias cached judgments).
    pub id: String,
    /// The search string.
    pub query: String,
    /// Optional schema filter passed to search.
    #[serde(default)]
    pub domain: Option<String>,
    /// Calibration marker. `None` = not a calibration query. `Some(map)` = a
    /// calibration query; the map is note-path -> graded human label (0..3) and
    /// may be empty while awaiting labels (filled via `--emit-calibration`).
    #[serde(default)]
    pub calibration: Option<BTreeMap<String, u8>>,
}

impl Queries {
    /// Load and validate the query set: parse YAML, reject duplicate ids and
    /// out-of-range calibration scores.
    pub fn load(path: &Path) -> Result<Self> {
        tracing::debug!(path = %path.display(), "Queries::load");
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading eval query set {}", path.display()))?;
        let parsed: Queries =
            serde_yaml::from_str(&text).with_context(|| format!("parsing eval query set {}", path.display()))?;

        if parsed.queries.is_empty() {
            bail!("eval query set {} contains no queries", path.display());
        }
        let mut seen = HashSet::new();
        for q in &parsed.queries {
            if !seen.insert(q.id.as_str()) {
                bail!("duplicate query id in {}: {}", path.display(), q.id);
            }
            for (note, score) in q.calibration.iter().flatten() {
                if *score > 3 {
                    bail!(
                        "calibration score for {note} in query {} is {score}; must be 0..3",
                        q.id
                    );
                }
            }
        }
        tracing::debug!(count = parsed.queries.len(), "Queries::load parsed");
        Ok(parsed)
    }

    /// Calibration queries (marked with a `calibration:` key, labeled or not).
    pub fn calibration(&self) -> impl Iterator<Item = &EvalQuery> {
        self.queries.iter().filter(|q| q.calibration.is_some())
    }

    /// Hand-labeled `(note_path, human_score)` pairs across all calibration
    /// queries (only the non-empty maps contribute).
    pub fn labeled(&self) -> impl Iterator<Item = (&str, &BTreeMap<String, u8>)> {
        self.queries.iter().filter_map(|q| match &q.calibration {
            Some(m) if !m.is_empty() => Some((q.id.as_str(), m)),
            _ => None,
        })
    }
}

#[cfg(test)]
mod tests;
