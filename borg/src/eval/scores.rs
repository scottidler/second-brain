//! The `eval_scores` table: one row per `(trace, run, check)` in
//! `eval-cache.db`, beside the judgment cache.
//!
//! The cache answers "have I paid for this exact judgment"; this table answers
//! "what is each trace's score, and is the trace scored yet". The two are
//! separate on purpose: the cache is keyed per judge call, this table per
//! check, and only this one carries error rows and run lineage.
//!
//! Rules (design: `2026-10-14-automated-note-quality-loop.md`, Architecture):
//! - **Current row:** for a trace and a required check, the `baseline` row whose
//!   `content_hash` is the trace's current distill hash and whose
//!   `rubric_version` is the check's current version. Older rows are history.
//! - **Covered:** every required check has a current row with `error IS NULL`.
//!   An error row, or a row from a judge outside the required set, never
//!   covers a trace, so the next run retries it.

use std::collections::BTreeSet;
use std::path::Path;

use eyre::{Context, Result};
use rusqlite::{Connection, params};

use super::cache::{CLAIMS_RUBRIC_VERSION, RUBRIC_VERSION};
use super::report::TraceRecord;
use super::{CHECK_JUDGE_CLAIMS, CHECK_JUDGE_DISTILLATION, RUN_KIND_BASELINE};

/// Rubric version of the deterministic checks. Bump when a check's metric
/// semantics change, so older rows stop being current.
pub const DETERMINISTIC_RUBRIC_VERSION: &str = "v1";

pub const CHECK_QUOTE_GROUNDING: &str = "quote-grounding";
pub const CHECK_TOOLS_GROUNDING: &str = "tools-grounding";
pub const CHECK_PLACEHOLDER_ARTIFACTS: &str = "placeholder-artifacts";
pub const CHECK_LISTICLE_SURVIVAL: &str = "listicle-survival";
pub const CHECK_NOTE_SIZE: &str = "note-size";

/// Every deterministic check, in the order rows are written. Each trace gets a
/// row for every one (`{"applicable":false}` when the note has nothing for the
/// check to measure), so a trace can be covered without a quote or a listicle.
pub const DETERMINISTIC_CHECKS: [&str; 5] = [
    CHECK_QUOTE_GROUNDING,
    CHECK_TOOLS_GROUNDING,
    CHECK_PLACEHOLDER_ARTIFACTS,
    CHECK_LISTICLE_SURVIVAL,
    CHECK_NOTE_SIZE,
];

/// `judge_model` of a deterministic check's row: no model made the call.
pub const NO_MODEL: &str = "";

const CREATE_TABLE: &str = "CREATE TABLE IF NOT EXISTS eval_scores (
    trace           TEXT NOT NULL,
    run_kind        TEXT NOT NULL,
    run_id          TEXT NOT NULL,
    kind            TEXT NOT NULL,
    judge_model     TEXT NOT NULL,
    check_name      TEXT NOT NULL,
    rubric_version  TEXT NOT NULL,
    content_hash    TEXT NOT NULL,
    scored_at       TEXT NOT NULL,
    metrics_json    TEXT NOT NULL,
    error           TEXT,
    PRIMARY KEY (trace, run_kind, run_id, content_hash, rubric_version, judge_model, check_name)
);";

/// One check a trace must have a current, error-free row for to be covered.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RequiredCheck {
    /// `""` for a deterministic check.
    pub judge_model: String,
    pub check_name: String,
    pub rubric_version: String,
}

/// The required-check set: every deterministic check plus both judge checks of
/// every model in `judge_models` (`eval.judge-models`).
pub fn required_checks(judge_models: &[String]) -> Vec<RequiredCheck> {
    let deterministic = DETERMINISTIC_CHECKS.iter().map(|check| RequiredCheck {
        judge_model: NO_MODEL.to_string(),
        check_name: (*check).to_string(),
        rubric_version: DETERMINISTIC_RUBRIC_VERSION.to_string(),
    });
    let judged = judge_models.iter().flat_map(|model| {
        [
            (CHECK_JUDGE_DISTILLATION, RUBRIC_VERSION),
            (CHECK_JUDGE_CLAIMS, CLAIMS_RUBRIC_VERSION),
        ]
        .into_iter()
        .map(move |(check, version)| RequiredCheck {
            judge_model: model.clone(),
            check_name: check.to_string(),
            rubric_version: version.to_string(),
        })
    });
    deterministic.chain(judged).collect()
}

/// One `eval_scores` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoreRow {
    pub trace: String,
    pub run_kind: String,
    pub run_id: String,
    pub kind: String,
    pub judge_model: String,
    pub check_name: String,
    pub rubric_version: String,
    pub content_hash: String,
    /// RFC 3339, comparable with `julianday()`.
    pub scored_at: String,
    pub metrics_json: String,
    /// Non-null when the judge call failed; such a row never covers a trace.
    pub error: Option<String>,
}

/// SQLite-backed score table.
pub struct ScoreStore {
    conn: Connection,
}

impl ScoreStore {
    /// Open (creating parent dirs and the table if needed). Safe beside an open
    /// [`super::cache::JudgmentCache`] on the same file.
    pub fn open(path: &Path) -> Result<Self> {
        log::debug!("ScoreStore::open: path={}", path.display());
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("creating eval cache dir {}", parent.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening eval scores {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .context("setting eval scores busy timeout")?;
        conn.execute_batch(CREATE_TABLE).context("creating eval_scores table")?;
        Ok(Self { conn })
    }

    /// Open an in-memory store (tests).
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("opening in-memory eval scores")?;
        conn.execute_batch(CREATE_TABLE).context("creating eval_scores table")?;
        Ok(Self { conn })
    }

    /// Persist a row. The seven-part key makes a re-score of the same
    /// trace, content, rubric, model and check an overwrite, with one guard: an
    /// error row never replaces a row that succeeded. A transient judge failure
    /// on a `--rebuild-cache` re-run must not un-score a trace.
    pub fn put(&self, row: &ScoreRow) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO eval_scores
                 (trace, run_kind, run_id, kind, judge_model, check_name, rubric_version,
                  content_hash, scored_at, metrics_json, error)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT (trace, run_kind, run_id, content_hash, rubric_version, judge_model, check_name)
                 DO UPDATE SET kind = excluded.kind, scored_at = excluded.scored_at,
                               metrics_json = excluded.metrics_json, error = excluded.error
                 WHERE excluded.error IS NULL OR eval_scores.error IS NOT NULL",
                params![
                    row.trace,
                    row.run_kind,
                    row.run_id,
                    row.kind,
                    row.judge_model,
                    row.check_name,
                    row.rubric_version,
                    row.content_hash,
                    row.scored_at,
                    row.metrics_json,
                    row.error,
                ],
            )
            .with_context(|| format!("inserting eval score {} {}", row.trace, row.check_name))?;
        Ok(())
    }

    /// Every row for `trace`, oldest key first (tests and reports).
    pub fn rows_for(&self, trace: &str) -> Result<Vec<ScoreRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT trace, run_kind, run_id, kind, judge_model, check_name, rubric_version,
                        content_hash, scored_at, metrics_json, error
                 FROM eval_scores WHERE trace = ?1
                 ORDER BY run_kind, run_id, content_hash, rubric_version, judge_model, check_name",
            )
            .context("preparing eval score read")?;
        let rows = stmt
            .query_map(params![trace], |r| {
                Ok(ScoreRow {
                    trace: r.get(0)?,
                    run_kind: r.get(1)?,
                    run_id: r.get(2)?,
                    kind: r.get(3)?,
                    judge_model: r.get(4)?,
                    check_name: r.get(5)?,
                    rubric_version: r.get(6)?,
                    content_hash: r.get(7)?,
                    scored_at: r.get(8)?,
                    metrics_json: r.get(9)?,
                    error: r.get(10)?,
                })
            })
            .context("reading eval scores")?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .context("reading an eval score row")
    }

    /// True when every check in `required` has a current, error-free baseline
    /// row for `trace` at `content_hash`. An empty `required` covers nothing:
    /// a loop that requires no check has no definition of "scored".
    pub fn is_covered(&self, trace: &str, content_hash: &str, required: &[RequiredCheck]) -> Result<bool> {
        if required.is_empty() {
            return Ok(false);
        }
        let mut stmt = self
            .conn
            .prepare(
                "SELECT judge_model, check_name, rubric_version FROM eval_scores
                 WHERE trace = ?1 AND run_kind = ?2 AND run_id = '' AND content_hash = ?3
                   AND error IS NULL",
            )
            .context("preparing eval coverage read")?;
        let have: BTreeSet<RequiredCheck> = stmt
            .query_map(params![trace, RUN_KIND_BASELINE, content_hash], |r| {
                Ok(RequiredCheck {
                    judge_model: r.get(0)?,
                    check_name: r.get(1)?,
                    rubric_version: r.get(2)?,
                })
            })
            .context("reading eval coverage")?
            .collect::<std::result::Result<_, _>>()
            .context("reading an eval coverage row")?;
        Ok(required.iter().all(|check| have.contains(check)))
    }
}

/// The `eval_scores` rows for one evaluated trace.
///
/// Deterministic rows are always produced. Judge rows are produced only when
/// `judged` and only for a check that ran: a verdict becomes a success row, a
/// failed call becomes an error row, a check that never ran leaves no row.
pub fn rows_from_record(record: &TraceRecord, judged: bool, scored_at: &str) -> Vec<ScoreRow> {
    let row =
        |judge_model: &str, check: &str, rubric: &str, metrics: serde_json::Value, error: Option<String>| ScoreRow {
            trace: record.trace.clone(),
            run_kind: record.run_kind.clone(),
            run_id: record.run_id.clone(),
            kind: record.kind.clone(),
            judge_model: judge_model.to_string(),
            check_name: check.to_string(),
            rubric_version: rubric.to_string(),
            content_hash: record.content_hash.clone(),
            scored_at: scored_at.to_string(),
            metrics_json: metrics.to_string(),
            error,
        };
    let not_applicable = || serde_json::json!({ "applicable": false });
    let m = &record.metrics;
    let deterministic =
        |check: &str, metrics: serde_json::Value| row(NO_MODEL, check, DETERMINISTIC_RUBRIC_VERSION, metrics, None);

    let mut rows = vec![
        deterministic(
            CHECK_QUOTE_GROUNDING,
            m.quote_grounding.as_ref().map_or_else(
                not_applicable,
                |g| serde_json::json!({ "grounded": g.grounded, "quoted": g.quoted }),
            ),
        ),
        deterministic(
            CHECK_TOOLS_GROUNDING,
            m.tools_grounding
                .as_ref()
                .map_or_else(not_applicable, |t| serde_json::to_value(t).unwrap_or_default()),
        ),
        deterministic(
            CHECK_PLACEHOLDER_ARTIFACTS,
            serde_json::json!({ "reasons": m.placeholder_artifacts }),
        ),
        deterministic(
            CHECK_LISTICLE_SURVIVAL,
            m.listicle_survival
                .map_or_else(not_applicable, |score| serde_json::json!({ "score": score })),
        ),
        deterministic(
            CHECK_NOTE_SIZE,
            serde_json::json!({ "rendered_bytes": m.rendered_bytes, "within_ceiling": m.within_ceiling }),
        ),
    ];

    if judged {
        let error_of = |check: &str| record.errors.iter().find(|e| e.check == check).map(|e| e.error.clone());
        if let Some(axes) = &m.judge_distillation {
            let mut metrics = serde_json::to_value(axes).unwrap_or_default();
            metrics["source_truncated"] = record.source_truncated.into();
            rows.push(row(
                &record.judge_model,
                CHECK_JUDGE_DISTILLATION,
                RUBRIC_VERSION,
                metrics,
                None,
            ));
        } else if let Some(error) = error_of(CHECK_JUDGE_DISTILLATION) {
            rows.push(row(
                &record.judge_model,
                CHECK_JUDGE_DISTILLATION,
                RUBRIC_VERSION,
                serde_json::json!({}),
                Some(error),
            ));
        }
        if let Some(verdicts) = &m.judge_claims {
            rows.push(row(
                &record.judge_model,
                CHECK_JUDGE_CLAIMS,
                CLAIMS_RUBRIC_VERSION,
                serde_json::json!({ "verdicts": verdicts }),
                None,
            ));
        } else if let Some(error) = error_of(CHECK_JUDGE_CLAIMS) {
            rows.push(row(
                &record.judge_model,
                CHECK_JUDGE_CLAIMS,
                CLAIMS_RUBRIC_VERSION,
                serde_json::json!({}),
                Some(error),
            ));
        }
    }
    rows
}

#[cfg(test)]
mod tests;
