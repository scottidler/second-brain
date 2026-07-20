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
///
/// `role`/`text` are DEFENSIVELY `Option<String>`: clyde constructs them
/// non-null today (`clyde/sessions/src/db/query.rs:205`), so this is NOT a
/// present-null the contract emits - it is future-malformed-element tolerance
/// on the `--with-body` path, so one absent role/text degrades to an empty
/// string in the canonical body render rather than aborting a re-appearance
/// hash. (Same discipline as the record-level per-record parse resilience.)
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct BodyMessage {
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub subagent: bool,
}

/// One session record from a bulk-metadata page or an `--id` export.
///
/// Several fields carry contract semantics that are easy to get wrong and are
/// called out explicitly:
/// - `host` and `scope` stay NON-null: clyde always emits `host` and
///   re-derives `scope` via `scope::classify(cwd)`
///   (`clyde/sessions/src/export.rs:116-120`), so both are always present.
/// - `cwd`, `created`, `title`, `first_prompt` are PRESENT-NULL: clyde emits
///   JSON `null` for all four on untitled / one-shot / empty / never-touched
///   sessions (`export.rs`). `Option<String>` + `#[serde(default)]` handles
///   both present-null and (defensively) omitted. Before this relaxation, one
///   null-string field in ANY session aborted the entire batch parse
///   (harvest-completion design, Problem #1). A null/unparseable `created`
///   additionally gets a selection-stage rejection (`select.rs`) so it never
///   reaches clustering's `parse_ts` (which errors the whole plan).
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
    /// Present-null: the session's working directory, or `null` on an
    /// empty/never-touched session. NOT omitted.
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub project_dir: Option<String>,
    /// Present-null: `Some("<org>/<repo>")` or `null` when cwd has no repo
    /// anchor. NOT omitted.
    #[serde(default)]
    pub repo: Option<String>,
    /// Present-null, same shape as `repo` (e.g. `"main"`, `"HEAD"`, or null).
    #[serde(default)]
    pub git_branch: Option<String>,
    /// Present-null: RFC-3339 creation time, or `null` on an empty session. A
    /// null/unparseable value is rejected at the selection stage before it can
    /// reach clustering.
    #[serde(default)]
    pub created: Option<String>,
    pub modified: String,
    #[serde(default)]
    pub updated_at: Option<i64>,
    #[serde(default)]
    pub duration_secs: Option<i64>,
    pub dormant: bool,
    /// Present-null: the session title, or `null` when untitled. The publish
    /// path falls back to `Session <id>` when this is null/empty.
    #[serde(default)]
    pub title: Option<String>,
    /// Present-null: the first user prompt, or `null` on an empty session.
    #[serde(default)]
    pub first_prompt: Option<String>,
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

/// The export envelope with `sessions` left as raw JSON values, so each record
/// can be deserialized element-by-element and one malformed element degrades to
/// a skipped record + a [`ParseRejection`] rather than aborting the whole
/// batch (design doc Alternative 3: per-record resilience over widen-types-only).
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct RawExport {
    schema_version: u32,
    #[serde(default)]
    generated_at: Option<String>,
    #[serde(default)]
    host: Option<String>,
    cursor: i64,
    sessions: Vec<serde_json::Value>,
}

/// One `sessions[]` element that failed per-record deserialization in
/// [`parse_export`]. Carried out of the parse boundary so the LIVE harvest path
/// can mint a DURABLE `received->rejected` receipt (keyed by `session_id`)
/// BEFORE the watermark advances - a skipped record is never silently lost
/// (design doc Resolved Decision: "Per-record parse skip must be DURABLE").
#[derive(Debug, Clone, PartialEq)]
pub struct ParseRejection {
    /// The `session-id` recovered from the malformed element by parsing it as a
    /// bare `serde_json::Value` first (clyde always emits `session-id`, so it is
    /// recoverable even when the record as a whole fails to deserialize).
    /// `None` only when the id itself is unreadable.
    pub session_id: Option<String>,
    /// 0-based index of the element within the `sessions` array. The positional
    /// fallback identifier when `session_id` is unreadable (the per-element
    /// deserialize seam has no raw byte offset to report).
    pub index: usize,
    /// The serde error that rejected the element.
    pub reason: String,
}

/// The result of parsing a clyde export: the well-formed records (as a
/// [`SessionExport`]) plus any per-record parse rejections that were skipped.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedExport {
    pub export: SessionExport,
    pub rejections: Vec<ParseRejection>,
}

/// Parse a `clyde session export` payload, asserting schema-version 1. The
/// envelope-level failure modes (unparseable JSON, wrong version) stay loud,
/// FAIL-CLOSED errors - harvest never returns an empty result on a bad
/// contract, and a wrong MAJOR still refuses the whole run. Individual
/// malformed `sessions[]` elements, however, are SKIPPED (logged WARN, carried
/// out as [`ParseRejection`]s) so one unexpected null does not nuke the batch.
pub fn parse_export(bytes: &[u8]) -> Result<ParsedExport> {
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
    let raw: RawExport =
        serde_json::from_slice(bytes).context("clyde session export: failed to parse the schema-version-1 envelope")?;

    let mut records: Vec<SessionRecord> = Vec::with_capacity(raw.sessions.len());
    let mut rejections: Vec<ParseRejection> = Vec::new();
    for (index, value) in raw.sessions.into_iter().enumerate() {
        // Recover `session-id` BEFORE consuming the value, so a malformed
        // record is still keyable to a durable receipt.
        let session_id = value.get("session-id").and_then(|v| v.as_str()).map(str::to_string);
        match serde_json::from_value::<SessionRecord>(value) {
            Ok(rec) => records.push(rec),
            Err(err) => {
                match &session_id {
                    Some(id) => log::warn!(
                        "harvest::parse_export: skipping malformed record session_id={id} index={index}: {err}"
                    ),
                    None => log::warn!(
                        "harvest::parse_export: skipping malformed record (unreadable session-id) index={index}: {err}"
                    ),
                }
                rejections.push(ParseRejection {
                    session_id,
                    index,
                    reason: format!("malformed session record: {err}"),
                });
            }
        }
    }

    let export = SessionExport {
        schema_version: raw.schema_version,
        generated_at: raw.generated_at,
        host: raw.host,
        cursor: raw.cursor,
        sessions: records,
    };
    log::debug!(
        "harvest::parse_export: parsed cursor={} sessions={} parse_rejections={}",
        export.cursor,
        export.sessions.len(),
        rejections.len()
    );
    Ok(ParsedExport { export, rejections })
}

#[cfg(test)]
mod tests;
