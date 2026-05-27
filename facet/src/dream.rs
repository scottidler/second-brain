//! Dream: derived, regenerable enrichment artifact.
//!
//! Per the design doc and Architect Round 2 consensus, dreams have NO
//! SQLite table. Each dream pass queries `gems` and `narratives`
//! in-memory, produces `Dream` variants, and renders them directly to
//! markdown under `notes/facet/dreams/`. If a dream pass crashes, the
//! next pass produces the same findings from the same canonical
//! inputs.
//!
//! This module owns only the enum definition. The dream pass itself
//! (semantic dedup, cross-references, stale-spectrum detection,
//! narrative-candidate proposals) lands in Phase 6.

use serde::{Deserialize, Serialize};

pub mod discover;
pub mod render;
pub mod run;

#[cfg(test)]
mod tests;

/// One dream-finding. Each variant carries the citations and the
/// proposed enrichment; the renderer decides how to present each kind
/// under `notes/facet/dreams/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Dream {
    /// Multiple gems describe the same concept across sessions.
    /// `canonical` is the gem_id the dream pass picks as the
    /// representative; `gem_ids` is the full set (including
    /// `canonical`).
    SemanticDuplicateGroup { gem_ids: Vec<i64>, canonical: i64 },

    /// Gem A's review references the same constraint as gem B's task,
    /// or otherwise points at gem B (precursor / follow-up / etc.).
    /// `relation` is free-form (e.g. "precursor", "fixed-by").
    CrossReference {
        from_gem: i64,
        to_gem: i64,
        relation: String,
    },

    /// A narrative's cluster has grown since the narrative was last
    /// written; the narrative needs revision. `new_gem_ids_since`
    /// lists the gem ids added to the cluster after the narrative's
    /// `synthesised_at`.
    StaleSpectrum {
        narrative_id: i64,
        new_gem_ids_since: Vec<i64>,
    },

    /// A cluster has reached threshold size but has not yet been
    /// narrated. The dream proposes a title and thesis so the operator
    /// can fast-track the narrate pass.
    NarrativeCandidate {
        gem_ids: Vec<i64>,
        proposed_title: String,
        proposed_thesis: String,
    },
}
