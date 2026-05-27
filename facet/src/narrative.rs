//! Narrative: the discovery-side of a spectrum.
//!
//! A narrative is a synthesised story over a cluster of gems: a
//! Session Arc, a Cross-Session Arc, or an evergreen mode rollup
//! (Phase 5). One narrative renders into one
//! `notes/facet/spectra/<slug>.md` file.
//!
//! Submodules (Phase 5):
//! - [`discover`]: find candidate clusters (Session Arc + Cross-Session
//!   Arc + evergreen mode rollups)
//! - [`narrate`]: invoke `facet-narrate.md` per cluster, parse JSON,
//!   honour the rejection gate
//! - [`render`]: write the per-narrative markdown note to
//!   `notes/facet/spectra/<slug>.md`

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub mod discover;
pub mod narrate;
pub mod render;
pub mod run;

#[cfg(test)]
mod tests;

/// One narrative spectrum. `id` is 0 for not-yet-persisted narratives;
/// the ledger fills it in on upsert.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Narrative {
    #[serde(default)]
    pub id: i64,
    pub slug: String,
    pub title: String,
    pub thesis: String,
    pub body_md: String,
    pub gem_ids: Vec<i64>,
    pub axes: NarrativeAxes,
    pub synthesised_at: DateTime<Utc>,
    pub synthesiser_model: String,
    #[serde(default = "default_revision")]
    pub revision: u32,
}

fn default_revision() -> u32 {
    1
}

/// What holds a cluster together. Populated by the discovery pass; the
/// synthesis pass cites it so the narrative can explain why the cluster
/// exists.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NarrativeAxes {
    #[serde(default)]
    pub semantic_cluster_id: Option<i64>,
    #[serde(default)]
    pub mode_mix: Vec<(String, u32)>,
    #[serde(default)]
    pub time_window: Option<(DateTime<Utc>, DateTime<Utc>)>,
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default)]
    pub workitem_ids: Vec<i64>,
}

/// The narrative-discovery archetype. Two real shapes plus an
/// evergreen back-compat shape. Phase 5 implements all three.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Archetype {
    /// Chronological run inside a single session, no clustering.
    Session,
    /// HDBSCAN cluster across sessions, chronologically ordered.
    CrossSession,
    /// Synthetic mode-bucket rollup (back-compat with v1 spectra).
    Evergreen,
}

impl Archetype {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::CrossSession => "cross-session",
            Self::Evergreen => "evergreen",
        }
    }
}

/// Operator-editable status, mirrored from the spectrum note's
/// frontmatter (`facet-spectrum-status`). The narrate pass reads this
/// to decide whether to suppress regeneration of a rejected cluster
/// (per the operator-rejection mechanism in Phase 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SpectrumStatus {
    Active,
    Rejected,
}

impl SpectrumStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Rejected => "rejected",
        }
    }
}
