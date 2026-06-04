//! Receipts: the failure-stage taxonomy and the shared path helper for borg's
//! receipts SQLite database.
//!
//! The receipts log is borg's durable record of every input it ever sees: one
//! row per `trace_id`, mutated in place from `received` to `succeeded` or
//! `failed` at terminal time. The concrete SQLite code lives in
//! `borg::receipts`; only the cross-crate types and the file-path helper live
//! here so that vault, borg, oracle, and the `sb` CLI all agree on the same
//! values without duplicating logic.

use std::path::PathBuf;
use std::str::FromStr;

use eyre::{Result, eyre};

/// One of the seven terminal failure classifications a trace can land in.
///
/// Replaces the older `DlqStage` enum from `vault::dlq`. The variant set is
/// identical except `WatchdogOrphan` is renamed `Crashed` to match the
/// receipts schema's `failure_stage` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailureStage {
    /// Filtered or rejected at the intake door (disallowed chat, unsupported
    /// media, bad payload).
    IntakeRejected,
    /// The classifier could not figure out what to do with the input.
    ClassifyFailed,
    /// Network fetch / extractor failure (fabric + jina both failed,
    /// blocklist hit, 4xx/5xx).
    FetchFailed,
    /// Quality gate refused to publish the produced note.
    QualityBlocked,
    /// `PIPELINE_HARD_TIMEOUT_SECS` elapsed before publish.
    PipelineTimedOut,
    /// `write_atomic` failure when publishing the final note.
    PublishFailed,
    /// Background watchdog detected a `received` row that never produced a
    /// terminal event within the deadline window.
    Crashed,
}

impl FailureStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::IntakeRejected => "intake-rejected",
            Self::ClassifyFailed => "classify-failed",
            Self::FetchFailed => "fetch-failed",
            Self::QualityBlocked => "quality-blocked",
            Self::PipelineTimedOut => "pipeline-timed-out",
            Self::PublishFailed => "publish-failed",
            Self::Crashed => "crashed",
        }
    }

    /// Every variant, in declaration order. Used by tests, schema CHECK
    /// constraint verifiers, and CLI grouping helpers.
    pub fn all() -> &'static [Self] {
        &[
            Self::IntakeRejected,
            Self::ClassifyFailed,
            Self::FetchFailed,
            Self::QualityBlocked,
            Self::PipelineTimedOut,
            Self::PublishFailed,
            Self::Crashed,
        ]
    }
}

impl std::fmt::Display for FailureStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for FailureStage {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "intake-rejected" => Ok(Self::IntakeRejected),
            "classify-failed" => Ok(Self::ClassifyFailed),
            "fetch-failed" => Ok(Self::FetchFailed),
            "quality-blocked" => Ok(Self::QualityBlocked),
            "pipeline-timed-out" => Ok(Self::PipelineTimedOut),
            "publish-failed" => Ok(Self::PublishFailed),
            "crashed" => Ok(Self::Crashed),
            _ => Err(format!("unknown failure stage: {s}")),
        }
    }
}

/// Coarse classification of the receipts row's raw input. Matches the
/// `kind` column in the SQLite schema. Distinct from `vault::intake::IntakeKind`
/// (which is the front-door classification, recorded before any pipeline
/// work runs and including richer kinds like `sticker` or `poll` that the
/// receipts table flattens to `binary`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReceiptKind {
    /// `raw_input` is the literal URL.
    Url,
    /// `raw_input` is the literal text body.
    Text,
    /// `raw_input` is a short structured descriptor; the actual bytes live
    /// in the `system/intake/<trace_id>.txt` sidecar.
    Binary,
}

impl ReceiptKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Url => "url",
            Self::Text => "text",
            Self::Binary => "binary",
        }
    }
}

impl std::fmt::Display for ReceiptKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for ReceiptKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "url" => Ok(Self::Url),
            "text" => Ok(Self::Text),
            "binary" => Ok(Self::Binary),
            _ => Err(format!("unknown receipt kind: {s}")),
        }
    }
}

/// Lifecycle status of a receipts row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReceiptStatus {
    Received,
    Succeeded,
    Failed,
}

impl ReceiptStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Received => "received",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }
}

impl std::fmt::Display for ReceiptStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for ReceiptStatus {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "received" => Ok(Self::Received),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            _ => Err(format!("unknown receipt status: {s}")),
        }
    }
}

/// Subdirectory under `dirs::data_local_dir()` that owns borg's data files.
pub const SB_BORG_DATA_DIR: &str = "sb/borg";

/// Resolve the platform-native path to `~/.local/share/sb/borg/receipts.db`
/// (or macOS equivalent). The directory may not exist yet; the caller is
/// responsible for `create_dir_all` before opening the DB.
pub fn receipts_db_path() -> Result<PathBuf> {
    let data = dirs::data_local_dir()
        .ok_or_else(|| eyre!("dirs::data_local_dir() returned None (set HOME or XDG_DATA_HOME)"))?;
    Ok(data.join(SB_BORG_DATA_DIR).join("receipts.db"))
}

/// Resolve the directory containing the receipts DB. Useful for the bootstrap
/// step that has to `create_dir_all` before any open call.
pub fn receipts_dir() -> Result<PathBuf> {
    let data = dirs::data_local_dir()
        .ok_or_else(|| eyre!("dirs::data_local_dir() returned None (set HOME or XDG_DATA_HOME)"))?;
    Ok(data.join(SB_BORG_DATA_DIR))
}

#[cfg(test)]
mod tests;
