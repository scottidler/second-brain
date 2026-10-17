//! `sb borg eval --unscored`: score the successful ingests that are not yet
//! covered, oldest first, and record every result in `eval_scores`.
//!
//! The backlog is driven by receipts, not a time window: every `succeeded`
//! receipt whose trace is still in staging and is not covered (see
//! [`super::scores`]) is owed a score, so a run capped by `--limit` leaves the
//! rest for the next one instead of skipping it.
//!
//! A succeeded receipt whose trace is not in staging (swept after the 14-day
//! window, or a kind the eval does not score) cannot be scored from staging
//! and is counted as `not_staged`, never as owed: scoring reads staging, and
//! counting the unscorable would pin `--unscored --count` above zero forever.

use std::collections::BTreeMap;
use std::path::Path;

use eyre::{Context, Result};
use rusqlite::Connection;

use super::report::EvalReport;
use super::scores::{RequiredCheck, ScoreStore, rows_from_record};
use super::{ClaimJudge, DistillationJudge, EvalOpts, EvalOutcome, cache, evaluate, judged_input, load_staged_trace};

/// Traces of `succeeded` receipts, oldest terminal first (a receipt with no
/// terminal time sorts by when it was received).
pub fn succeeded_traces(receipts: &Connection) -> Result<Vec<String>> {
    let mut stmt = receipts
        .prepare(
            "SELECT trace_id FROM receipts WHERE status = 'succeeded'
             ORDER BY COALESCE(terminal_at, received_at) ASC, trace_id ASC",
        )
        .context("preparing succeeded-receipts read")?;
    stmt.query_map([], |r| r.get::<_, String>(0))
        .context("reading succeeded receipts")?
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("reading a succeeded receipt")
}

/// The traces still owed a score, and what was left out of that count.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Backlog {
    /// Uncovered, staged traces, oldest first.
    pub traces: Vec<String>,
    /// Succeeded receipts whose trace is not in staging (or not a scored kind).
    pub not_staged: usize,
    /// Staged traces whose artifacts exist but do not parse; named on stderr by
    /// the caller, never silently dropped.
    pub unreadable: Vec<(String, String)>,
}

/// Find the uncovered traces, stopping after `limit` when given. Without a
/// limit it walks every succeeded receipt, which is what `--count` needs.
pub fn find_backlog(
    receipts: &Connection,
    store: &ScoreStore,
    staged_root: &Path,
    required: &[RequiredCheck],
    limit: Option<usize>,
) -> Result<Backlog> {
    log::debug!(
        "eval::backlog::find_backlog: root={} required={} limit={limit:?}",
        staged_root.display(),
        required.len()
    );
    let mut backlog = Backlog::default();
    for trace in succeeded_traces(receipts)? {
        if limit.is_some_and(|n| backlog.traces.len() >= n) {
            break;
        }
        let fixture = match load_staged_trace(staged_root, &trace) {
            Ok(Some(fixture)) => fixture,
            Ok(None) => {
                backlog.not_staged += 1;
                continue;
            }
            Err(e) => {
                backlog.unreadable.push((trace, format!("{e:#}")));
                continue;
            }
        };
        let hash = judged_input(&fixture).content_hash;
        if !store.is_covered(&trace, &hash, required)? {
            backlog.traces.push(trace);
        }
    }
    log::debug!(
        "eval::backlog::find_backlog: owed={} not_staged={} unreadable={}",
        backlog.traces.len(),
        backlog.not_staged,
        backlog.unreadable.len()
    );
    Ok(backlog)
}

/// Fail when a var `llm.api-keys` names does not resolve here. The map says
/// which vendors the judges need; a timer run whose environment lacks one would
/// otherwise burn every trace into an error row and look like a vendor outage.
/// Names the vendor and var, never a value.
pub fn preflight_keys(keys: &vault::fabric::ApiKeys) -> Result<()> {
    let missing: Vec<String> = keys
        .iter()
        .filter(|(_, var)| vault::config::resolve_secret(var).is_err())
        .map(|(vendor, var)| format!("{vendor:?} ('{var}')"))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    eyre::bail!(
        "llm.api-keys names vars that do not resolve in this environment: {}; \
         the service's env-bootstrap must carry them",
        missing.join(", ")
    )
}

/// Everything one `--unscored` run produced.
pub struct UnscoredRun {
    /// Traces selected from the backlog for this run.
    pub selected: usize,
    pub not_staged: usize,
    pub unreadable: Vec<(String, String)>,
    /// One report per judge model, in `eval.judge-models` order.
    pub reports: Vec<EvalReport>,
    /// `eval_scores` rows written (deterministic rows repeat per judge model
    /// and overwrite themselves).
    pub rows_written: usize,
}

impl UnscoredRun {
    /// The one failing run: judges ran and every judged trace of every model
    /// errored. A partly failing run succeeds; its error rows leave the traces
    /// uncovered so the next run retries them.
    pub fn all_errored(&self) -> bool {
        !self.reports.is_empty() && self.reports.iter().all(EvalReport::all_judged_errored)
    }
}

/// Builds the two judges for one model's `opts` (production: fabric; tests:
/// mocks).
pub type JudgeFactory<'a> = &'a dyn Fn(&EvalOpts) -> Result<(Box<dyn DistillationJudge>, Box<dyn ClaimJudge>)>;

/// Score the backlog. `opts` carries the shared judge settings (keys, patterns
/// dir); `judge_models` and `limit` come from `eval:` config. An empty
/// `judge_models` is an error, not a default.
#[allow(clippy::too_many_arguments)]
pub fn run_unscored(
    receipts: &Connection,
    store: &ScoreStore,
    cache: &cache::JudgmentCache,
    opts: &EvalOpts,
    staged_root: &Path,
    judge_models: &[String],
    limit: Option<usize>,
    judges: JudgeFactory,
) -> Result<UnscoredRun> {
    if judge_models.is_empty() {
        eyre::bail!(
            "eval.judge-models is empty: name the judge models a trace must be scored by in borg.yml \
             (the required-check set is derived from it)"
        );
    }
    let required = super::scores::required_checks(judge_models);
    let backlog = find_backlog(receipts, store, staged_root, &required, limit)?;
    let mut fixtures = Vec::with_capacity(backlog.traces.len());
    for trace in &backlog.traces {
        if let Some(fixture) =
            load_staged_trace(staged_root, trace).with_context(|| format!("re-reading staged trace {trace}"))?
        {
            fixtures.push(fixture);
        }
    }
    let mut run = UnscoredRun {
        selected: fixtures.len(),
        not_staged: backlog.not_staged,
        unreadable: backlog.unreadable,
        reports: Vec::new(),
        rows_written: 0,
    };
    if fixtures.is_empty() {
        return Ok(run);
    }

    for model in judge_models {
        let model_opts = EvalOpts {
            judge_model: model.clone(),
            no_judge: false,
            emit_calibration: None,
            ..opts.clone()
        };
        let (judge, claim_judge) = judges(&model_opts)?;
        let outcome = evaluate(&fixtures, &BTreeMap::new(), cache, &*judge, &*claim_judge, &model_opts)?;
        let EvalOutcome::Report(report) = outcome else {
            eyre::bail!("evaluate returned a calibration sheet for an --unscored run");
        };
        let scored_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        for record in &report.traces {
            for row in rows_from_record(record, true, &scored_at) {
                store.put(&row)?;
                run.rows_written += 1;
            }
        }
        run.reports.push(*report);
    }
    Ok(run)
}

#[cfg(test)]
mod tests;
