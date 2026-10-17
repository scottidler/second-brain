//! Doctor checks: the automated note-quality loop (`sb borg eval`).
//!
//! One line states where the loop stands: traces scored, failing, judge
//! checks trusted, remediations, the newest weekly report. Warns when an
//! untrusted judge check is silently downgrading the loop, when a remediated
//! note still fails after its one re-distill, and when the weekly report has
//! stopped arriving. Reads the same stores as `sb borg eval --weekly`, with
//! no model call.

use chrono::NaiveDate;

use borg::eval::weekly::{self, Health};

use super::Finding;

/// The doctor findings for the eval loop. Nothing scored yet is Info (a fresh
/// install); a store that cannot be read is a Warn, never an Error.
pub(super) fn findings(config: &borg::config::Config) -> Vec<Finding> {
    let cache_path = vault::paths::borg_eval_cache_path();
    if !cache_path.exists() {
        return vec![Finding::info("eval: no scores yet (eval-cache.db does not exist)")];
    }
    match read_health(config, &cache_path) {
        Ok(health) => eval_findings(
            &health,
            config.eval.report.stale_after_days,
            chrono::Local::now().date_naive(),
        ),
        Err(e) => vec![Finding::warn(
            format!("eval: health query failed: {e:#}"),
            "sb borg eval --weekly reads the same stores and prints the failing step",
        )],
    }
}

fn read_health(config: &borg::config::Config, cache_path: &std::path::Path) -> eyre::Result<Health> {
    let receipts = borg::receipts::open_default()?;
    let store = borg::eval::scores::ScoreStore::open(cache_path)?;
    let trust_rows = borg::eval::trust::TrustStore::open(cache_path)?.all()?;
    let reports_dir = vault::paths::borg_eval_reports_dir();
    let staged_root = vault::paths::borg_stages_dir();
    weekly::health(&weekly::Inputs {
        receipts: &receipts,
        store: &store,
        staged_root: &staged_root,
        eval: &config.eval,
        trust_rows: &trust_rows,
        reports_dir: &reports_dir,
    })
}

/// Pure half of [`findings`]: the eval line, then each Warn condition.
pub(super) fn eval_findings(h: &Health, stale_after_days: i64, today: NaiveDate) -> Vec<Finding> {
    if h.scored == 0 {
        return vec![Finding::info(
            "eval: no traces scored yet (sb-eval.timer fills eval_scores daily)",
        )];
    }
    let report = h.last_report.map_or_else(|| "never".to_string(), |d| d.to_string());
    let mut findings = vec![Finding::info(format!(
        "eval: {} traces scored, {} failing ({} actionable), {} of {} judge checks trusted, {} remediated ({} still failing), last weekly report {report}",
        h.scored,
        h.failing,
        h.actionable,
        h.judge_checks - h.untrusted_checks.len(),
        h.judge_checks,
        h.remediated,
        h.still_failing,
    ))];
    if !h.untrusted_checks.is_empty() {
        findings.push(Finding::warn(
            format!(
                "eval: {} of {} judge checks are untrusted, so no remediation or candidate acts on them: {}",
                h.untrusted_checks.len(),
                h.judge_checks,
                h.untrusted_checks.join(", ")
            ),
            "sb borg eval --inject re-measures them (trust needs the paired catch rate on every class a check can see)",
        ));
    }
    if h.still_failing > 0 {
        findings.push(Finding::warn(
            format!(
                "eval: {} remediated note(s) still fail after their one re-distill (they stay searchable)",
                h.still_failing
            ),
            "the Remediated section of the newest report in ~/.local/share/sb/borg/eval-reports/ names each",
        ));
    }
    match h.last_report {
        None => findings.push(Finding::warn(
            "eval: no weekly report has been written",
            "systemctl --user status sb-eval-report.timer; sb borg eval --weekly runs one now",
        )),
        Some(date) if (today - date).num_days() > stale_after_days => findings.push(Finding::warn(
            format!(
                "eval: the newest weekly report is {} days old ({date}; stale after {stale_after_days})",
                (today - date).num_days()
            ),
            "systemctl --user status sb-eval-report.timer; sb borg eval --weekly runs one now",
        )),
        Some(_) => {}
    }
    findings
}

#[cfg(test)]
mod tests;
