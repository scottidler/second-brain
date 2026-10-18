//! `sb borg eval --weekly`: the weekly failure-cluster report.
//!
//! Design: `docs/design/2026-10-14-automated-note-quality-loop.md` (Failure
//! analysis; Remediation trail; "What an untrusted judge is barred from").
//!
//! One run gathers the current failing rows ([`remediate::plan`]), makes ONE
//! clustering call over them ([`cluster`]), reads the remediation claims
//! ([`remediated`]), each judge check's trust state ([`trust`]) and the
//! candidate files, and renders `eval-reports/<date>.md`. [`Report::message`]
//! is the single Telegram summary. [`health`] is the same gather without the
//! model call, for `sb doctor`. Nothing here writes the vault.

pub mod cluster;
pub mod remediated;

use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use eyre::{Context, Result};
use rusqlite::Connection;

use super::candidate::file::CandidateFile;
use super::remediate::{self, Plan};
use super::scores::{ScoreStore, required_checks};
use super::trust::{TrustRow, TrustState, judge_check_trust};
use super::{CHECK_JUDGE_CLAIMS, load_staged_trace};
use crate::config::EvalConfig;
use cluster::{Clusterer, Clustering, TraceFailures, claim_excerpt};
use remediated::{Outcome, Remediation};

/// Telegram refuses a message over 4096 characters.
const MESSAGE_MAX_CHARS: usize = 3500;
/// Claim texts shown per failing trace.
const CLAIMS_PER_TRACE: usize = 5;
/// Categories named in the Telegram summary.
const MESSAGE_CATEGORIES: usize = 3;

/// What a gather reads.
pub struct Inputs<'a> {
    pub receipts: &'a Connection,
    pub store: &'a ScoreStore,
    pub staged_root: &'a Path,
    pub eval: &'a EvalConfig,
    pub trust_rows: &'a [TrustRow],
    /// `eval-reports/`: reports are `<date>.md`, candidates live in `candidates/`.
    pub reports_dir: &'a Path,
}

/// One judge check's trust state.
#[derive(Debug, Clone, PartialEq)]
pub struct TrustLine {
    pub judge_model: String,
    pub check: String,
    pub state: TrustState,
}

/// A candidate file, read back.
#[derive(Debug, Clone, PartialEq)]
pub enum CandidateLine {
    Measured {
        path: PathBuf,
        pattern: String,
        created: String,
        train: (f64, f64),
        holdout: (f64, f64),
        holdout_wins: bool,
    },
    Unreadable {
        path: PathBuf,
        error: String,
    },
}

/// Everything the report and the doctor line read, with no model call.
#[derive(Debug)]
pub struct Gathered {
    pub plan: Plan,
    pub trust: Vec<TrustLine>,
    pub remediated: Vec<Remediation>,
    pub candidates: Vec<CandidateLine>,
}

/// Read the stores. Errors when a store cannot be read; an empty store is empty data.
pub fn gather(inputs: &Inputs<'_>) -> Result<Gathered> {
    log::debug!(
        "eval::weekly::gather: judge_models={:?} reports_dir={}",
        inputs.eval.judge_models,
        inputs.reports_dir.display()
    );
    if inputs.eval.judge_models.is_empty() {
        eyre::bail!("eval.judge-models is empty: the required-check set the report reads is derived from it");
    }
    let plan = remediate::plan(
        inputs.receipts,
        inputs.store,
        inputs.staged_root,
        &inputs.eval.judge_models,
        inputs.trust_rows,
        &inputs.eval.trust,
    )?;
    let required = required_checks(&inputs.eval.judge_models);
    let trust = required
        .iter()
        .filter(|c| !c.judge_model.is_empty())
        .map(|c| TrustLine {
            judge_model: c.judge_model.clone(),
            check: c.check_name.clone(),
            state: judge_check_trust(&c.judge_model, &c.check_name, inputs.trust_rows, &inputs.eval.trust),
        })
        .collect();
    let remediated = remediated::collect(
        inputs.receipts,
        inputs.store,
        inputs.staged_root,
        &required,
        inputs.eval.claims_source_max_chars,
    )?;
    let candidates = read_candidates(&inputs.reports_dir.join("candidates"))?;
    Ok(Gathered {
        plan,
        trust,
        remediated,
        candidates,
    })
}

/// Every `*.md` candidate file under `dir`, oldest name first. A missing
/// directory is no candidates; a file that does not parse is listed as such.
fn read_candidates(dir: &Path) -> Result<Vec<CandidateLine>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    let mut paths: Vec<PathBuf> = entries
        .map(|e| e.map(|e| e.path()))
        .collect::<std::result::Result<_, _>>()
        .with_context(|| format!("reading {}", dir.display()))?;
    paths.retain(|p| p.extension().is_some_and(|x| x == "md"));
    paths.sort();
    let mut lines = Vec::new();
    for path in paths {
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        lines.push(match CandidateFile::parse(&text) {
            Ok(file) => CandidateLine::Measured {
                pattern: file.pattern.clone(),
                created: file.created.get(..10).unwrap_or(&file.created).to_string(),
                train: (file.train.baseline.score, file.train.candidate.score),
                holdout: (file.holdout.baseline.score, file.holdout.candidate.score),
                holdout_wins: file.holdout.candidate_wins(),
                path,
            },
            Err(e) => CandidateLine::Unreadable {
                path,
                error: format!("{e:#}"),
            },
        });
    }
    Ok(lines)
}

impl Gathered {
    /// Failing traces.
    pub fn failing(&self) -> usize {
        self.plan.candidates.len()
    }

    /// Failing traces some trigger may act on (a deterministic or trusted check).
    pub fn actionable(&self) -> usize {
        self.plan.candidates.iter().filter(|c| c.trigger().is_some()).count()
    }

    fn untrusted_checks(&self) -> Vec<&TrustLine> {
        self.trust
            .iter()
            .filter(|t| matches!(t.state, TrustState::Untrusted(_)))
            .collect()
    }

    fn outcome_count(&self, outcome: Outcome) -> usize {
        self.remediated.iter().filter(|r| r.outcome == outcome).count()
    }

    /// What the model reads: one block per failing trace, capped at `max`
    /// (in trace-id order). The second value is how many were left out.
    fn failures_for_model(&self, inputs: &Inputs<'_>, max: usize) -> Result<(Vec<TraceFailures>, usize)> {
        let left_out = self.plan.candidates.len().saturating_sub(max);
        let mut out = Vec::new();
        for candidate in self.plan.candidates.iter().take(max) {
            let mut entries = Vec::new();
            let mut fixture = None;
            for f in &candidate.failures {
                let who = if f.judge_model.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", f.judge_model)
                };
                let gate = if f.gate.may_act() { "" } else { " [untrusted judge]" };
                entries.push(format!("{}{who}{gate}: {}", f.check, f.reason));
                if f.check == CHECK_JUDGE_CLAIMS {
                    if fixture.is_none() {
                        fixture = Some(load_staged_trace(inputs.staged_root, &candidate.trace)?);
                    }
                    if let Some(Some(fixture)) = &fixture {
                        entries.extend(rejected_claims(inputs.store, fixture, &candidate.trace, f)?);
                    }
                }
            }
            out.push(TraceFailures {
                trace: candidate.trace.clone(),
                kind: candidate.kind.clone(),
                entries,
            });
        }
        Ok((out, left_out))
    }
}

/// The claim texts the failing `judge-claims` row marked not supported, read
/// back from the staged note (verdict `i` is `distilled.claims[i]`).
fn rejected_claims(
    store: &ScoreStore,
    fixture: &super::Fixture,
    trace: &str,
    failure: &remediate::Failure,
) -> Result<Vec<String>> {
    let rows = store.baseline_rows_at(trace, &failure.content_hash)?;
    let Some(row) = rows.iter().find(|r| {
        r.check_name == CHECK_JUDGE_CLAIMS
            && r.judge_model == failure.judge_model
            && r.rubric_version == failure.rubric_version
            && r.error.is_none()
    }) else {
        return Ok(Vec::new());
    };
    let metrics: serde_json::Value = serde_json::from_str(&row.metrics_json)
        .with_context(|| format!("trace {trace}: unreadable judge-claims metrics"))?;
    let verdicts = metrics
        .get("verdicts")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| eyre::eyre!("trace {trace}: judge-claims metrics carry no verdicts"))?;
    let mut texts = Vec::new();
    for (i, verdict) in verdicts.iter().enumerate() {
        let verdict = verdict.as_str().unwrap_or("");
        if verdict == "supported" {
            continue;
        }
        if let Some(claim) = fixture.distilled.claims.get(i) {
            texts.push(format!("{verdict} claim: {}", claim_excerpt(&claim.text)));
        }
    }
    texts.truncate(CLAIMS_PER_TRACE);
    Ok(texts)
}

/// The finished weekly report.
#[derive(Debug)]
pub struct Report {
    pub date: NaiveDate,
    /// The model the clustering call used, for the header.
    pub model: String,
    pub judge_models: Vec<String>,
    pub gathered: Gathered,
    pub clustering: Clustering,
    /// Failing traces the clustering call was not shown (over `max-failures`).
    pub left_out: usize,
}

/// Gather, then make the one clustering call. A failing clustering call is an
/// error: no report is better than one with categories the model never made.
pub fn build(inputs: &Inputs<'_>, clusterer: &dyn Clusterer, model: &str, date: NaiveDate) -> Result<Report> {
    let gathered = gather(inputs)?;
    let (shown, left_out) = gathered.failures_for_model(inputs, inputs.eval.report.max_failures)?;
    let clustering = cluster::cluster_failures(clusterer, &shown)?;
    Ok(Report {
        date,
        model: model.to_string(),
        judge_models: inputs.eval.judge_models.clone(),
        gathered,
        clustering,
        left_out,
    })
}

impl Report {
    /// `<dir>/<date>.md`.
    pub fn path_in(&self, dir: &Path) -> PathBuf {
        dir.join(format!("{}.md", self.date))
    }

    /// Write the rendered report to [`Report::path_in`], replacing the day's
    /// earlier report.
    pub fn write(&self, dir: &Path) -> Result<PathBuf> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let path = self.path_in(dir);
        std::fs::write(&path, self.render()).with_context(|| format!("writing {}", path.display()))?;
        Ok(path)
    }

    pub fn render(&self) -> String {
        let g = &self.gathered;
        let mut o = format!("# Eval report {}\n\n", self.date);
        o.push_str(&format!(
            "Judges: {}. Clustering model: {}. Failing means a current, error-free row of a required check \
             fails; kappa and the planted-error rates live in `sb borg eval --inject`.\n\n",
            self.judge_models.join(", "),
            if self.model.is_empty() { "fabric default" } else { &self.model }
        ));

        o.push_str("## Summary\n\n");
        o.push_str(&format!("- scored traces: {}\n", g.plan.scored));
        o.push_str(&format!(
            "- failing traces: {} ({} actionable, {} flagged only by untrusted judge checks)\n",
            g.failing(),
            g.actionable(),
            g.failing() - g.actionable()
        ));
        o.push_str(&format!(
            "- scored traces no longer in staging: {}; with no succeeded receipt here: {}; unreadable: {}\n",
            g.plan.not_staged,
            g.plan.no_receipt,
            g.plan.unreadable.len()
        ));
        if self.left_out > 0 {
            o.push_str(&format!(
                "- the clustering call was shown {} failing traces; {} more were not\n",
                g.failing() - self.left_out,
                self.left_out
            ));
        }
        o.push('\n');

        o.push_str("## Judge trust\n\n");
        o.push_str("An untrusted judge check records scores but triggers no remediation and feeds no applied candidate. Deterministic checks make no model call and are never gated.\n\n");
        o.push_str("| judge | check | state | why |\n|---|---|---|---|\n");
        for t in &g.trust {
            let (state, why) = match &t.state {
                TrustState::Trusted => ("trusted".to_string(), String::new()),
                TrustState::Ungated => ("ungated".to_string(), String::new()),
                TrustState::Untrusted(failures) => (
                    "UNTRUSTED".to_string(),
                    failures
                        .iter()
                        .map(|f| format!("{}: {}", f.class.as_str(), f.reason))
                        .collect::<Vec<_>>()
                        .join("; "),
                ),
            };
            o.push_str(&format!("| {} | {} | {state} | {why} |\n", t.judge_model, t.check));
        }
        o.push('\n');

        o.push_str("## Failure categories\n\n");
        if self.clustering.clusters.is_empty() {
            o.push_str("No failing traces this week.\n\n");
        }
        for (i, c) in self.clustering.clusters.iter().enumerate() {
            o.push_str(&format!("### {}. {} ({} traces)\n\n", i + 1, c.name, c.traces.len()));
            if !c.description.is_empty() {
                o.push_str(&format!("{}\n\n", c.description));
            }
            o.push_str(&format!("Examples: {}\n\n", c.examples().join(", ")));
        }
        if self.clustering.unknown_ids > 0 {
            o.push_str(&format!(
                "The model named {} trace ids that were not in its input; they were ignored.\n\n",
                self.clustering.unknown_ids
            ));
        }

        o.push_str("## Remediated\n\n");
        if g.remediated.is_empty() {
            o.push_str("No source has been remediated.\n\n");
        } else {
            for r in &g.remediated {
                o.push_str(&format!(
                    "- `{}` -> `{}`: **{}**\n  - source: {}\n  - before: {}\n  - after: {}\n",
                    r.original_trace,
                    r.remediation_trace.as_deref().unwrap_or("(no trace)"),
                    r.outcome.as_str(),
                    r.source_url,
                    r.before,
                    r.after
                ));
            }
            o.push('\n');
        }

        o.push_str("## Candidates\n\n");
        if g.candidates.is_empty() {
            o.push_str("No measured candidate patterns.\n");
        }
        for c in &g.candidates {
            match c {
                CandidateLine::Measured {
                    path,
                    pattern,
                    created,
                    train,
                    holdout,
                    holdout_wins,
                } => o.push_str(&format!(
                    "- `{pattern}` ({created}): train {:.4} -> {:.4}, holdout {:.4} -> {:.4} ({})\n  - apply: `sb borg eval --apply-candidate {}`\n",
                    train.0,
                    train.1,
                    holdout.0,
                    holdout.1,
                    if *holdout_wins { "wins on the holdout" } else { "does not win on the holdout" },
                    path.display()
                )),
                CandidateLine::Unreadable { path, error } => {
                    o.push_str(&format!("- `{}` is unreadable: {error}\n", path.display()));
                }
            }
        }
        o
    }

    /// The one Telegram message of the run: counts, the biggest categories,
    /// remediation and trust in a few lines, and where the report is.
    pub fn message(&self, report_path: &Path) -> String {
        let g = &self.gathered;
        let mut m = format!(
            "sb eval {}: {} failing of {} scored ({} actionable)\n",
            self.date,
            g.failing(),
            g.plan.scored,
            g.actionable()
        );
        let top: Vec<String> = self
            .clustering
            .clusters
            .iter()
            .take(MESSAGE_CATEGORIES)
            .map(|c| format!("{} ({})", c.name, c.traces.len()))
            .collect();
        if !top.is_empty() {
            m.push_str(&format!("Top: {}\n", top.join("; ")));
        }
        m.push_str(&format!(
            "Remediated: {} ({} passed, {} still failing)\n",
            g.remediated.len(),
            g.outcome_count(Outcome::Passed),
            g.outcome_count(Outcome::StillFailing)
        ));
        m.push_str(&format!(
            "Untrusted judge checks: {} of {}\n",
            g.untrusted_checks().len(),
            g.trust.len()
        ));
        m.push_str(&format!("Report: {}", report_path.display()));
        m.chars().take(MESSAGE_MAX_CHARS).collect()
    }
}

/// What `sb doctor` reads about the eval loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Health {
    pub scored: usize,
    pub failing: usize,
    pub actionable: usize,
    pub judge_checks: usize,
    /// `judge-claims (gemini-2.5-pro)` and the like.
    pub untrusted_checks: Vec<String>,
    pub remediated: usize,
    pub still_failing: usize,
    pub last_report: Option<NaiveDate>,
}

/// [`gather`] without the model call, plus the newest report's date.
pub fn health(inputs: &Inputs<'_>) -> Result<Health> {
    let g = gather(inputs)?;
    Ok(Health {
        scored: g.plan.scored,
        failing: g.failing(),
        actionable: g.actionable(),
        judge_checks: g.trust.len(),
        untrusted_checks: g
            .untrusted_checks()
            .iter()
            .map(|t| format!("{} ({})", t.check, t.judge_model))
            .collect(),
        remediated: g.remediated.len(),
        still_failing: g.outcome_count(Outcome::StillFailing),
        last_report: latest_report_date(inputs.reports_dir)?,
    })
}

/// The newest `<YYYY-MM-DD>.md` in `dir`; a missing dir has none, and a file
/// not named like a report is not one.
pub fn latest_report_date(dir: &Path) -> Result<Option<NaiveDate>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    let mut newest = None;
    for entry in entries {
        let name = entry.with_context(|| format!("reading {}", dir.display()))?.file_name();
        let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".md")) else {
            continue;
        };
        if let Ok(date) = NaiveDate::parse_from_str(stem, "%Y-%m-%d") {
            newest = newest.max(Some(date));
        }
    }
    Ok(newest)
}

#[cfg(test)]
mod tests;
