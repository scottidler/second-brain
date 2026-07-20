//! The clyde `session export` contract, schema-version 1
//! (harvest-clyde-sessions design; companion contract
//! `clyde/docs/design/2026-07-17-session-export-contract.md`).
//!
//! These types model the JSON payload clyde emits. Harvest consumes ONLY this
//! versioned contract - never `sessions.db`, never the raw transcript
//! `.jsonl`. [`parse_export`] is the single loud boundary: an unparseable
//! payload or a schema-version mismatch is a hard error, never an empty result
//! (design doc: "harvest never limps along on a mismatched contract").
//!
//! Forward-compatibility: the record type is deliberately NOT
//! `#[serde(deny_unknown_fields)]`. The contract is designed to gain fields
//! additively WITHIN schema-version 1 (clyde's files-touched branch adds
//! `repos-touched`/`files-touched` without bumping the export version). The
//! schema-version assertion, not field-set rigidity, is the real gate - this
//! is the "forward-compatible envelope stays tolerant" carve-out from the Rust
//! serde rules, documented here where it lives.

use eyre::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// The only contract version harvest speaks. A payload carrying any other
/// value is a loud failure in [`parse_export`].
pub const CONTRACT_SCHEMA_VERSION: u32 = 1;

/// The frozen `enrich-status` vocabulary (design doc Selection section:
/// `ok | skipped-personal | skipped-empty | failed | null`). Modeled as an
/// enum rather than a free string so a typo/unknown value fails deserialization
/// loudly instead of silently missing the `ok` selection signal. The `null`
/// case is the `Option` wrapper on the field, not a variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnrichStatus {
    Ok,
    SkippedPersonal,
    SkippedEmpty,
    Failed,
}

/// One role-labeled transcript message, present only in an `--id --with-body`
/// export. `subagent` marks a sub-agent turn (defaults false when omitted).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct BodyMessage {
    pub role: String,
    pub text: String,
    #[serde(default)]
    pub subagent: bool,
}

/// One session record from a bulk-metadata page or an `--id` export.
///
/// Three fields carry contract semantics that are easy to get wrong and are
/// called out explicitly:
/// - `repo` and `git_branch` are PRESENT-NULL (the key is present with a
///   `null` value, not omitted) when clyde finds no `~/repos/<org>/<repo>`
///   anchor. `Option<String>` + `#[serde(default)]` handles both present-null
///   and (defensively) omitted.
/// - `repos_touched` is THREE-STATE (`Option<Vec<String>>`): `None` =
///   omitted/unknowable (key absent, transcript reaped), `Some(vec![])` =
///   parsed but no repo path resolved, `Some(xs)` = the touched set. A
///   default-empty `Vec` would collapse the first two and fabricate a
///   definitive "touched nothing" from missing data - WRONG. Phase 3 only
///   carries this faithfully; the bridge consumer is Phases 9-13.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct SessionRecord {
    pub session_id: String,
    pub host: String,
    pub scope: String,
    pub cwd: String,
    #[serde(default)]
    pub project_dir: Option<String>,
    /// Present-null: `Some("<org>/<repo>")` or `null` when cwd has no repo
    /// anchor. NOT omitted.
    #[serde(default)]
    pub repo: Option<String>,
    /// Present-null, same shape as `repo` (e.g. `"main"`, `"HEAD"`, or null).
    #[serde(default)]
    pub git_branch: Option<String>,
    pub created: String,
    pub modified: String,
    #[serde(default)]
    pub updated_at: Option<i64>,
    #[serde(default)]
    pub duration_secs: Option<i64>,
    pub dormant: bool,
    pub title: String,
    #[serde(default)]
    pub first_prompt: String,
    pub n_msgs: i64,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// `null`/omitted -> `None`; a real status -> `Some(..)`.
    #[serde(default)]
    pub enrich_status: Option<EnrichStatus>,
    #[serde(default)]
    pub redaction_count: i64,
    #[serde(default)]
    pub transcript_path: Option<String>,
    #[serde(default)]
    pub staged_path: Option<String>,
    #[serde(default)]
    pub archived: bool,
    /// THREE-STATE (see type docs). Omitted -> `None`.
    #[serde(default)]
    pub repos_touched: Option<Vec<String>>,
    /// Tolerated additive field (clyde's files-touched branch); Phase 3 does
    /// not consume it, but modeling it keeps the record honest.
    #[serde(default)]
    pub files_touched: Option<Vec<String>>,
    /// Present only with `--with-body`.
    #[serde(default)]
    pub body: Option<Vec<BodyMessage>>,
    #[serde(default)]
    pub body_truncated: bool,
    #[serde(default)]
    pub body_error: Option<String>,
}

impl SessionRecord {
    /// Short clyde pointer for this session, as it rides `source:` on a
    /// published note and keys forensic artifacts.
    pub fn clyde_uri(&self) -> String {
        format!("clyde://{}", self.session_id)
    }
}

/// The top-level export envelope. `cursor` is the opaque revision harvest
/// persists as its watermark; `sessions` is the page.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct SessionExport {
    pub schema_version: u32,
    #[serde(default)]
    pub generated_at: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    pub cursor: i64,
    pub sessions: Vec<SessionRecord>,
}

/// Minimal header used to check the contract version BEFORE the full parse, so
/// a version mismatch surfaces as a clear "unsupported version" error instead
/// of a confusing missing/renamed-field error deep in the payload.
#[derive(Deserialize)]
struct VersionHeader {
    #[serde(rename = "schema-version")]
    schema_version: u32,
}

/// Parse a `clyde session export` payload, asserting schema-version 1. Both
/// failure modes (unparseable JSON, wrong version) are loud errors - harvest
/// never returns an empty result on a bad contract.
pub fn parse_export(bytes: &[u8]) -> Result<SessionExport> {
    log::debug!("harvest::parse_export: input_bytes={}", bytes.len());
    let header: VersionHeader = serde_json::from_slice(bytes)
        .context("clyde session export: payload is not valid JSON or is missing `schema-version`")?;
    if header.schema_version != CONTRACT_SCHEMA_VERSION {
        bail!(
            "clyde session export: unsupported schema-version {} (harvest speaks {}); \
             refusing to limp along on a mismatched contract - upgrade the clyde binary or harvest",
            header.schema_version,
            CONTRACT_SCHEMA_VERSION
        );
    }
    let export: SessionExport =
        serde_json::from_slice(bytes).context("clyde session export: failed to parse the schema-version-1 payload")?;
    log::debug!(
        "harvest::parse_export: parsed cursor={} sessions={}",
        export.cursor,
        export.sessions.len()
    );
    Ok(export)
}

#[cfg(test)]
mod tests;
