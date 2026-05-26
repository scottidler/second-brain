//! Judgment-moment extraction. Per (session, cluster_assignment), mine
//! the moments of senior judgment from the bounded turn range.
//!
//! Phase 4 implementation. Phase 1 ships the `JudgmentMoment` row type
//! so the ledger schema can persist it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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
