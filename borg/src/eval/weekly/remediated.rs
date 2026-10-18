//! The weekly report's Remediated section: each remediated source, its
//! original and remediation traces, the frozen "before" score, the "after",
//! and one of six outcomes.
//!
//! The outcome comes from the remediation receipt's status, never from the POST
//! response: intake returns `Queued` and runs the ingest detached, so a lost
//! response is `pending` until the receipt settles. The "before" is the row the
//! claim froze (`trigger_*` columns, including a copy of its metrics), so a
//! later rescore of the original trace, which can overwrite the same
//! `eval_scores` key, never rewrites it.

use std::collections::BTreeSet;
use std::path::Path;

use eyre::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use crate::eval::fixtures::StagedTrace;
use crate::eval::remediate::row_failure;
use crate::eval::scores::{RequiredCheck, ScoreRow, ScoreStore};
use crate::eval::{REASON_SOURCE_OVER_CAP, RUN_KIND_BASELINE, judged_input, probe_staged_trace};
use crate::types::ReplayOrigin;

/// Where a remediation stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The remediation receipt is still `received`, or the POST has no trace yet.
    Pending,
    /// Scored, and every required check passes.
    Passed,
    /// Scored, and some required check still fails.
    StillFailing,
    /// The ingest succeeded and the trace is not covered yet.
    Unscored,
    /// The latest attempt to score it left only error rows.
    EvalErrored,
    /// The remediation receipt ended `failed` or `rejected`, or the POST never landed.
    AttemptFailed,
    /// Scored, nothing fails, but a required `judge-claims` row is not
    /// judgeable (source over the claims cap): never reported as passed.
    NotJudgeable,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Passed => "passed",
            Self::StillFailing => "still-failing",
            Self::Unscored => "unscored",
            Self::EvalErrored => "eval-errored",
            Self::AttemptFailed => "attempt-failed",
            Self::NotJudgeable => "not-judgeable",
        }
    }
}

/// One remediated source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remediation {
    pub source_url: String,
    pub original_trace: String,
    pub remediation_trace: Option<String>,
    pub outcome: Outcome,
    /// The frozen trigger row, in words.
    pub before: String,
    /// The remediated trace's scores, or why there are none.
    pub after: String,
}

struct Claim {
    source_url: String,
    original_trace: String,
    remediation_trace: Option<String>,
    trigger_judge_model: String,
    trigger_check: String,
    trigger_rubric: String,
    trigger_content_hash: String,
    trigger_metrics_json: Option<String>,
    post_error: Option<String>,
}

/// Every remediation claim, oldest first, classified. `required` is the
/// required-check set (`scores::required_checks(eval.judge-models)`);
/// `claims_cap` is `eval.claims-source-max-chars`.
pub fn collect(
    receipts: &Connection,
    store: &ScoreStore,
    staged_root: &Path,
    required: &[RequiredCheck],
    claims_cap: usize,
) -> Result<Vec<Remediation>> {
    log::debug!(
        "eval::weekly::remediated::collect: required={} claims_cap={claims_cap}",
        required.len()
    );
    let mut stmt = receipts
        .prepare(
            "SELECT source_url, original_trace, remediation_trace, trigger_judge_model, trigger_check,
                    trigger_rubric, trigger_content_hash, post_error, trigger_metrics_json
             FROM remediations ORDER BY claimed_at, source_url",
        )
        .context("preparing the remediations read")?;
    let claims = stmt
        .query_map([], |r| {
            Ok(Claim {
                source_url: r.get(0)?,
                original_trace: r.get(1)?,
                remediation_trace: r.get(2)?,
                trigger_judge_model: r.get(3)?,
                trigger_check: r.get(4)?,
                trigger_rubric: r.get(5)?,
                trigger_content_hash: r.get(6)?,
                post_error: r.get(7)?,
                trigger_metrics_json: r.get(8)?,
            })
        })
        .context("reading remediations")?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("reading a remediation claim")?;
    claims
        .iter()
        .map(|claim| classify(receipts, store, staged_root, required, claims_cap, claim))
        .collect()
}

fn classify(
    receipts: &Connection,
    store: &ScoreStore,
    staged_root: &Path,
    required: &[RequiredCheck],
    claims_cap: usize,
    claim: &Claim,
) -> Result<Remediation> {
    let before = before_text(claim)?;
    let remediation_trace = match &claim.remediation_trace {
        Some(trace) => Some(trace.clone()),
        None => receipts
            .query_row(
                "SELECT trace_id FROM receipts WHERE replay_of = ?1 AND replay_origin = ?2 LIMIT 1",
                params![claim.original_trace, ReplayOrigin::AutoRemediation.as_str()],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .with_context(|| format!("looking up the remediation of {}", claim.original_trace))?,
    };
    let done = |outcome: Outcome, after: String| Remediation {
        source_url: claim.source_url.clone(),
        original_trace: claim.original_trace.clone(),
        remediation_trace: remediation_trace.clone(),
        outcome,
        before: before.clone(),
        after,
    };
    let Some(trace) = remediation_trace.as_deref() else {
        return Ok(match &claim.post_error {
            Some(error) => done(
                Outcome::AttemptFailed,
                format!("the POST's outcome is unknown and no intake row exists: {error}"),
            ),
            None => done(Outcome::Pending, "the POST has returned no trace yet".to_string()),
        });
    };
    let Some(receipt) = crate::receipts::get(receipts, trace)? else {
        return Ok(done(Outcome::Pending, format!("no receipts row for {trace} yet")));
    };
    match receipt.status.as_str() {
        "received" => Ok(done(Outcome::Pending, "the ingest is still in flight".to_string())),
        "failed" | "rejected" => {
            let why = [receipt.failure_stage, receipt.failure_reason]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(": ");
            Ok(done(
                Outcome::AttemptFailed,
                format!("the remediation receipt is {}: {why}", receipt.status),
            ))
        }
        "succeeded" => {
            let (outcome, after) = scored(store, staged_root, required, claims_cap, trace)?;
            Ok(done(outcome, after))
        }
        other => eyre::bail!("remediation trace {trace} has an unknown receipt status '{other}'"),
    }
}

fn scored(
    store: &ScoreStore,
    staged_root: &Path,
    required: &[RequiredCheck],
    claims_cap: usize,
    trace: &str,
) -> Result<(Outcome, String)> {
    let fixture = match probe_staged_trace(staged_root, trace) {
        StagedTrace::Loaded(fixture) => fixture,
        StagedTrace::Absent => {
            return Ok((
                Outcome::Unscored,
                "the remediated trace is no longer in staging, so it cannot be scored".to_string(),
            ));
        }
        StagedTrace::Unreadable(why) => {
            return Ok((
                Outcome::Unscored,
                format!("the remediated trace's staged source is unreadable: {why}"),
            ));
        }
    };
    let hash = judged_input(&fixture).content_hash;
    let rows = store.baseline_rows_at(trace, &hash)?;
    if !store.is_covered(trace, &hash, required, claims_cap)? {
        let needed: BTreeSet<&RequiredCheck> = required.iter().collect();
        let has_ok = |check: &RequiredCheck| rows.iter().any(|r| r.error.is_none() && same_check(r, check));
        let errored = rows
            .iter()
            .any(|r| r.error.is_some() && needed.iter().any(|c| same_check(r, c) && !has_ok(c)));
        return Ok(if errored {
            (
                Outcome::EvalErrored,
                "the latest scoring attempt left error rows for a required check".to_string(),
            )
        } else {
            (
                Outcome::Unscored,
                "no current rows for every required check yet".to_string(),
            )
        });
    }
    let mut failing = Vec::new();
    let mut not_judgeable = Vec::new();
    for row in rows
        .iter()
        .filter(|r| r.error.is_none() && required.iter().any(|c| same_check(r, c)))
    {
        if let Some(reason) = row_failure(row)? {
            failing.push(format!("{}: {reason}", check_label(row)));
        } else if let Some(why) = not_judgeable_text(row) {
            not_judgeable.push(why);
        }
    }
    Ok(if !failing.is_empty() {
        (Outcome::StillFailing, failing.join("; "))
    } else if !not_judgeable.is_empty() {
        (Outcome::NotJudgeable, not_judgeable.join("; "))
    } else {
        (Outcome::Passed, format!("all {} required checks pass", required.len()))
    })
}

/// `judge-claims (<model>) not judgeable: source N chars > cap C` for a
/// `source-over-cap` row, `None` for any other row.
fn not_judgeable_text(row: &ScoreRow) -> Option<String> {
    let metrics: serde_json::Value = serde_json::from_str(&row.metrics_json).ok()?;
    if metrics.get("reason").and_then(serde_json::Value::as_str) != Some(REASON_SOURCE_OVER_CAP) {
        return None;
    }
    Some(format!(
        "{} not judgeable: source {} chars > cap {}",
        check_label(row),
        metrics["source_chars"],
        metrics["cap"]
    ))
}

fn same_check(row: &ScoreRow, check: &RequiredCheck) -> bool {
    row.judge_model == check.judge_model
        && row.check_name == check.check_name
        && row.rubric_version == check.rubric_version
}

fn check_label(row: &ScoreRow) -> String {
    if row.judge_model.is_empty() {
        row.check_name.clone()
    } else {
        format!("{} ({})", row.check_name, row.judge_model)
    }
}

/// The frozen trigger row, in words. Reads only the claim's own copy, never
/// `eval_scores`: a later write to the trigger's key (a rescore,
/// `--inject --rebuild-cache`) must not change what "before" says.
fn before_text(claim: &Claim) -> Result<String> {
    let Some(metrics_json) = &claim.trigger_metrics_json else {
        return Ok(format!(
            "{}: the trigger row was not frozen (claimed before the claim carried its metrics)",
            claim.trigger_check
        ));
    };
    let row = ScoreRow {
        trace: claim.original_trace.clone(),
        run_kind: RUN_KIND_BASELINE.to_string(),
        run_id: String::new(),
        kind: String::new(),
        judge_model: claim.trigger_judge_model.clone(),
        check_name: claim.trigger_check.clone(),
        rubric_version: claim.trigger_rubric.clone(),
        content_hash: claim.trigger_content_hash.clone(),
        scored_at: String::new(),
        metrics_json: metrics_json.clone(),
        error: None,
    };
    let reason = row_failure(&row)?.unwrap_or_else(|| "passes".to_string());
    Ok(format!("{}: {reason}", check_label(&row)))
}
