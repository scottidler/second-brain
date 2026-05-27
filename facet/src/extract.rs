//! Judgment-slice extraction. Per (session, cluster_assignment), mine
//! the moments / gems of senior judgment from the bounded turn range.
//!
//! Phase 3 of the v2 redesign reshaped this module into a thin dispatch
//! layer over two extractors:
//!
//! - [`v1`]: the legacy one-line `JudgmentMoment` extractor. Retained
//!   intact during cutover; selected by the `--v1` flag on
//!   `sb facet harvest`.
//! - [`v2`]: the multi-turn dialog-slice `Gem` extractor. Default for
//!   new harvests; produces `Vec<Gem>` per cluster_assignment.
//!
//! The `JudgmentMoment` / `ExtractOutput` / `ExtractedMoment` types
//! below are v1-shape; they stay at this level for back-compat with
//! existing callers and the spectrum (evergreen) renderer. They land
//! in `v1/` as part of Phase 7 cleanup.

pub mod spectrum;
pub mod v1;
pub mod v2;

#[doc(inline)]
pub use v1::mine;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One mined judgment moment, in its persistent ledger shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgmentMoment {
    pub id: i64,
    pub workitem_id: i64,
    pub session_uuid: String,
    pub turn_uuid: String,
    pub mode: String,
    pub ai_move: String,
    pub scott_move: String,
    pub quote_excerpt: String,
    pub why_it_matters: String,
    pub extracted_at: DateTime<Utc>,
    pub extractor_model: String,
}

/// Raw output shape the extract LLM returns. Deserialised from YAML.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExtractOutput {
    #[serde(default)]
    pub moments: Vec<ExtractedMoment>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExtractedMoment {
    pub turn_uuid: String,
    pub mode: String,
    pub ai_move: String,
    pub scott_move: String,
    pub quote_excerpt: String,
    pub why_it_matters: String,
}
