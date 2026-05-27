//! Narrative: the discovery-side of a spectrum.
//!
//! A narrative is a synthesised story over a cluster of gems: a
//! Session Arc or a Cross-Session Arc. One narrative renders into one
//! `notes/facet/spectra/<slug>.md` file.
//!
//! Submodules:
//! - [`discover`]: find candidate clusters (Session Arc + Cross-Session
//!   Arc)
//! - [`narrate`]: invoke `facet-narrate.md` per cluster, parse JSON,
//!   honour the rejection gate
//! - [`render`]: write the per-narrative markdown note to
//!   `notes/facet/spectra/<slug>.md`

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub mod discover;
pub mod narrate;
pub mod present;
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

/// The narrative-discovery archetype. Two shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Archetype {
    /// Chronological run inside a single session, no clustering.
    Session,
    /// HDBSCAN cluster across sessions, chronologically ordered.
    CrossSession,
}

impl Archetype {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::CrossSession => "cross-session",
        }
    }
}

/// Operator-editable status, mirrored from the spectrum note's
/// frontmatter (`facet-spectrum-status`). The narrate pass reads this
/// to decide whether to suppress regeneration of a rejected cluster.
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
