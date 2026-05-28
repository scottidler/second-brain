//! Typed records the ledger reads and writes.
//!
//! These mirror the design doc's Data Model section. The ledger row
//! decoders in `ledger::sessions`, `ledger::quarantine`, and
//! `ledger::work_items` build these by hand from `rusqlite::Row`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Tier-1 output: one row per Claude Code session JSONL file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_uuid: String,
    pub jsonl_path: PathBuf,
    pub jsonl_sha256: String,
    pub repo_slug: Option<String>,
    pub repo_path: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub design_doc_files: Vec<PathBuf>,
    pub skill_invocations: Vec<String>,
    pub interaction_normalized: String,
    pub summary_one_line: String,
    pub theme_tags: Vec<String>,
    pub design_doc_focus: Option<PathBuf>,
    pub is_orphan: bool,
    pub classified_at: DateTime<Utc>,
    pub classifier_model: String,
}

/// A session that failed to classify or normalize. Adds rather than
/// mutates: the same session can have multiple rows from multiple
/// runs if the reason changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuarantineRecord {
    pub id: i64,
    pub session_uuid: String,
    pub jsonl_path: PathBuf,
    pub reason: String,
    pub quarantined_at: DateTime<Utc>,
}

/// Standard quarantine reasons. Free-form strings are also allowed
/// (e.g. a multi-repo case), but the prebaked variants cover the
/// common cases and let the CLI's `quarantine list` group cleanly.
pub mod quarantine_reason {
    pub const UNRESOLVABLE_REPO: &str = "unresolvable-repo";
    pub const MALFORMED_JSONL: &str = "malformed-jsonl";
    pub const CLASSIFY_CALL_FAILED: &str = "classify-call-failed";
    pub const EMPTY_INTERACTION: &str = "empty-interaction";
    pub const MULTI_REPO: &str = "multi-repo";
    pub const REDACTED: &str = "redacted-content";
}

/// Cluster-stage key for a work-item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkItemKey {
    /// Hard-cluster: sessions share a `design_doc_focus` path.
    DesignDoc,
    /// Soft-cluster: agglomerative clustering above similarity threshold.
    Theme,
    /// Did not cluster with anything; or `is_orphan = true`.
    Singleton,
}

impl WorkItemKey {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DesignDoc => "design-doc",
            Self::Theme => "theme",
            Self::Singleton => "singleton",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "design-doc" => Some(Self::DesignDoc),
            "theme" => Some(Self::Theme),
            "singleton" => Some(Self::Singleton),
            _ => None,
        }
    }
}

/// A materialized work-item: one cluster of sessions that share a
/// distillable shape (one design doc, one theme, or one singleton).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkItem {
    pub id: i64,
    pub key_type: WorkItemKey,
    /// For `DesignDoc`: the design-doc path. For `Theme`: a synthesized
    /// cluster id of the form `theme-<sha256-12>`. For `Singleton`:
    /// the lone session_uuid.
    pub key_value: String,
    pub repo_slug: Option<String>,
    /// sha256 of sorted member session_uuids, hex-encoded. Stable
    /// identity across re-cluster passes; survives slug/title churn.
    pub content_hash: String,
    pub session_uuids: Vec<String>,
    pub time_start: DateTime<Utc>,
    pub time_end: DateTime<Utc>,
    pub aggregated_tags: Vec<String>,
    pub materialized_at: DateTime<Utc>,
}
