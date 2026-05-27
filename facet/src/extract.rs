//! Judgment-moment extraction. Per (session, cluster_assignment), mine
//! the moments of senior judgment from the bounded turn range.
//!
//! The extract LLM is fed a YAML digest of the cluster_assignment's
//! turn range (bounded by `first_turn_uuid` / `last_turn_uuid`) and
//! emits a list of [`JudgmentMoment`]-shaped rows. The function
//! persists each row + flips the `cluster_assignments.extracted` flag
//! to 1 in a single transaction, so retries are safe and other
//! work-items' extracts are unaffected by one failure.

pub mod mine;
pub mod spectrum;

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
