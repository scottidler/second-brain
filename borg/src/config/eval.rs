use serde::{Deserialize, Serialize};

use vault::systemd::EnvBootstrap;

/// Daily, after the 03:00 harvest has published its notes.
const DEFAULT_SCHEDULE: &str = "*-*-* 05:00:00";
/// Default `eval.trust.min-paired-catch-rate`: a policy floor, not proof.
pub const DEFAULT_MIN_PAIRED_CATCH_RATE: f64 = 0.80;
/// Default `eval.trust.min-samples`: applicable traces a class needs before
/// its rate can make a judge check trusted.
pub const DEFAULT_MIN_SAMPLES: usize = 30;
/// Default `eval.limit`: traces one timer run scores (Scott, 2026-10-17), so
/// a borg.yml without the line drains the backlog instead of scoring it all
/// in one run.
pub const DEFAULT_LIMIT: usize = 100;
/// Default `eval.inject-workers`: judge calls in flight during `--inject`.
const DEFAULT_INJECT_WORKERS: usize = 4;
/// Default `eval.candidate.max-coverage-cut`: a candidate pattern that cuts
/// claims per note or enumeration items per note by more than this share of
/// the baseline scores 0, so a pattern cannot win by saying less.
pub const DEFAULT_MAX_COVERAGE_CUT: f64 = 0.10;
/// Default `eval.candidate.distill-workers`: distills in flight while a
/// candidate pattern is scored.
const DEFAULT_CANDIDATE_DISTILL_WORKERS: usize = 4;
/// Default `eval.report.schedule`: Sunday morning, after the daily 05:00 eval
/// has scored the week's last ingests.
const DEFAULT_REPORT_SCHEDULE: &str = "Sun *-*-* 06:00:00";
/// Default `eval.report.max-failures`: failing traces one weekly clustering
/// call is shown; the rest are counted and named in the report.
const DEFAULT_REPORT_MAX_FAILURES: usize = 200;
/// Default `eval.report.stale-after-days`: `sb doctor` warns when the newest
/// weekly report is older than this (a weekly cadence plus a day of slack).
const DEFAULT_REPORT_STALE_AFTER_DAYS: i64 = 8;
/// The default judge: a Gemini model, because the distiller is Anthropic and
/// self-preference is a documented judge bias. First in the default
/// `judge-models`, and the model `sb borg eval --staged` judges with when
/// `--judge-model` is not given.
pub const DEFAULT_JUDGE_MODEL: &str = "gemini-2.5-pro";
/// The second judge: Anthropic, so `--inject` and the timer score every note
/// with two vendors and the report can print their kappa.
pub const SECOND_JUDGE_MODEL: &str = "claude-sonnet-5";

/// The `eval:` block of `borg.yml`: what the `sb-eval.timer` run scores
/// (`sb borg eval --unscored`).
///
/// `judge-models` is the required-check set's judge half: a trace is covered
/// only when every deterministic check and every judge check of every model
/// named here has a current, error-free row in `eval_scores`. The default is
/// [`DEFAULT_JUDGE_MODEL`] (Gemini) then [`SECOND_JUDGE_MODEL`] (Anthropic);
/// the first entry is the default judge. An explicitly empty list makes
/// `--unscored` and `--inject` fail loudly.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct EvalConfig {
    /// systemd `OnCalendar` expression for `sb-eval.timer`, the ONE value
    /// baked into the timer unit.
    pub schedule: String,
    /// Cap on traces one `--unscored` run scores; the rest stay in the
    /// backlog for the next run. Default [`DEFAULT_LIMIT`]; `null` scores the
    /// whole backlog.
    pub limit: Option<usize>,
    /// Judge models (fabric `-m`), each scoring every trace. The first is
    /// the default judge ([`EvalConfig::default_judge_model`]).
    pub judge_models: Vec<String>,
    /// Secret bootstrap for `sb-eval.service` (same shape as
    /// `harvest.env-bootstrap`): its own `env-file`, so the one-shot run never
    /// clobbers a daemon's captured environment. It must carry every var
    /// `llm.api-keys` names; installing the unit without it while
    /// `llm.api-keys` is set is an error, because the run would score with no
    /// keys.
    pub env_bootstrap: Option<EnvBootstrap>,
    /// The bar a judge check must clear on `sb borg eval --inject` before
    /// its failures may act (remediation, candidates).
    pub trust: TrustConfig,
    /// Traces judged in parallel by `sb borg eval --inject`, each worker with
    /// its own fabric children and cache connection.
    pub inject_workers: usize,
    /// How a candidate distill pattern is scored against the current one
    /// (`--candidate-pattern`, `--apply-candidate`).
    pub candidate: CandidateConfig,
    /// The weekly failure-cluster report (`sb borg eval --weekly`).
    pub report: ReportConfig,
}

/// The `eval.report:` block: the weekly failure-cluster report that
/// `sb-eval-report.timer` runs.
///
/// Like the eval timer, the timer bakes only `OnCalendar` (`schedule`);
/// `max-failures` and the model are read from borg.yml by the service's own
/// ExecStart at fire time.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct ReportConfig {
    /// systemd `OnCalendar` expression for `sb-eval-report.timer`.
    pub schedule: String,
    /// Fabric `-m` for the one clustering call. `None` uses the default judge
    /// (`eval.judge-models[0]`), so the call goes to a vendor `llm.api-keys`
    /// already carries a key for.
    pub model: Option<String>,
    /// Failing traces shown to the clustering call.
    pub max_failures: usize,
    /// `sb doctor` warns when the newest report is older than this many days.
    pub stale_after_days: i64,
}

impl Default for ReportConfig {
    fn default() -> Self {
        Self {
            schedule: DEFAULT_REPORT_SCHEDULE.to_string(),
            model: None,
            max_failures: DEFAULT_REPORT_MAX_FAILURES,
            stale_after_days: DEFAULT_REPORT_STALE_AFTER_DAYS,
        }
    }
}

/// The `eval.candidate:` block: the coverage floor of the candidate metric
/// (quote grounding x claim support x listicle survival) and the distill
/// parallelism while scoring.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct CandidateConfig {
    pub max_coverage_cut: f64,
    pub distill_workers: usize,
}

impl Default for CandidateConfig {
    fn default() -> Self {
        Self {
            max_coverage_cut: DEFAULT_MAX_COVERAGE_CUT,
            distill_workers: DEFAULT_CANDIDATE_DISTILL_WORKERS,
        }
    }
}

/// The `eval.trust:` block. A judge check is trusted when, on every planted
/// class it can see, its paired catch rate is at least
/// `min-paired-catch-rate` over at least `min-samples` applicable traces, on
/// the current rubric version. Both are policy floors: 24 of 30 caught has a
/// one-sided Wilson lower bound near 0.66.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct TrustConfig {
    pub min_paired_catch_rate: f64,
    pub min_samples: usize,
}

impl Default for TrustConfig {
    fn default() -> Self {
        Self {
            min_paired_catch_rate: DEFAULT_MIN_PAIRED_CATCH_RATE,
            min_samples: DEFAULT_MIN_SAMPLES,
        }
    }
}

impl EvalConfig {
    /// The default judge: the first of `judge-models`, or `None` when the
    /// list was explicitly emptied.
    pub fn default_judge_model(&self) -> Option<&str> {
        self.judge_models.first().map(String::as_str)
    }

    /// Structural checks, run at config load: a blank judge model, a rate
    /// outside 0..=1, a zero sample floor, or zero inject workers is a load
    /// error, never a silent clamp.
    pub fn validate(&self) -> eyre::Result<()> {
        if let Some(i) = self.judge_models.iter().position(|m| m.trim().is_empty()) {
            eyre::bail!(
                "eval.judge-models[{i}] is blank: a blank model runs fabric's default model, which no trust \
                 measurement can name; list each judge model explicitly"
            );
        }
        let rate = self.trust.min_paired_catch_rate;
        if !(0.0..=1.0).contains(&rate) {
            eyre::bail!("eval.trust.min-paired-catch-rate must be within 0..=1, got {rate}");
        }
        if self.trust.min_samples == 0 {
            eyre::bail!("eval.trust.min-samples must be at least 1");
        }
        if self.inject_workers == 0 {
            eyre::bail!("eval.inject-workers must be at least 1");
        }
        let cut = self.candidate.max_coverage_cut;
        if !(0.0..1.0).contains(&cut) {
            eyre::bail!("eval.candidate.max-coverage-cut must be within 0..1, got {cut}");
        }
        if self.candidate.distill_workers == 0 {
            eyre::bail!("eval.candidate.distill-workers must be at least 1");
        }
        if self.report.max_failures == 0 {
            eyre::bail!("eval.report.max-failures must be at least 1");
        }
        if self.report.stale_after_days < 1 {
            eyre::bail!("eval.report.stale-after-days must be at least 1");
        }
        Ok(())
    }
}

impl Default for EvalConfig {
    fn default() -> Self {
        Self {
            schedule: DEFAULT_SCHEDULE.to_string(),
            limit: Some(DEFAULT_LIMIT),
            judge_models: vec![DEFAULT_JUDGE_MODEL.to_string(), SECOND_JUDGE_MODEL.to_string()],
            env_bootstrap: None,
            trust: TrustConfig::default(),
            inject_workers: DEFAULT_INJECT_WORKERS,
            candidate: CandidateConfig::default(),
            report: ReportConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests;
