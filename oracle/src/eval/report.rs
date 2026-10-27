//! The eval report: per-mode metrics and latency, lift over baselines,
//! calibration panel, the fact-layer ablation and whether the configured
//! target's rerank stage ran — plus a plain-text renderer for `sb` to print.

use crate::eval::metrics::MetricMeans;
use crate::server::RerankRun;

/// Metrics for one search mode (or the ablation variant).
#[derive(Debug, Clone)]
pub struct ModeReport {
    pub mode: String,
    pub means: MetricMeans,
    /// Wall-clock retrieval time per query for this target.
    pub latency: Latency,
    /// What the rerank stage did, per query. `None` for a target whose path
    /// has no rerank stage (every legacy mode and the ablation).
    pub rerank: Option<RerankTally>,
}

/// Per-query wall-clock percentiles, nearest-rank, in milliseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Latency {
    pub p50_ms: f64,
    pub p95_ms: f64,
    /// Queries timed; `0` means no samples and the percentiles are meaningless.
    pub n: usize,
}

impl Latency {
    /// Nearest-rank p50 and p95 over `samples`.
    pub fn from_samples(mut samples: Vec<f64>) -> Self {
        samples.sort_by(f64::total_cmp);
        Self {
            p50_ms: nearest_rank(&samples, 50.0),
            p95_ms: nearest_rank(&samples, 95.0),
            n: samples.len(),
        }
    }
}

/// The nearest-rank percentile of ascending `sorted`: the smallest sample with
/// at least `p` percent of samples at or below it. `0.0` when empty.
fn nearest_rank(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// How many queries the configured target's rerank stage ran on, and why it
/// did not on the rest.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RerankTally {
    pub ran: usize,
    pub off: usize,
    pub failed_open: usize,
    pub disabled: usize,
}

impl RerankTally {
    pub fn from_runs(runs: impl Iterator<Item = RerankRun>) -> Self {
        let mut tally = Self::default();
        for run in runs {
            match run {
                RerankRun::Ran => tally.ran += 1,
                RerankRun::Off => tally.off += 1,
                RerankRun::FailedOpen => tally.failed_open += 1,
                RerankRun::Disabled => tally.disabled += 1,
            }
        }
        tally
    }

    fn total(&self) -> usize {
        self.ran + self.off + self.failed_open + self.disabled
    }

    /// The table cell: `off` when the stage is not enabled, `ran N/T` when it
    /// ran on every query, and `NOT RUN` with the counts otherwise, so a
    /// latched-off stage can never read as a rerank-on result.
    fn cell(&self) -> String {
        let total = self.total();
        if self.off == total {
            "off".to_string()
        } else if self.ran == total {
            format!("ran {}/{total}", self.ran)
        } else {
            format!("NOT RUN {}/{total}", total - self.ran)
        }
    }
}

/// Judge-vs-human calibration (Architect finding #3: report a panel, gate on the
/// hit-boundary precision/recall — not Cohen's kappa alone, which the
/// class-imbalanced pool deflates).
#[derive(Debug, Clone)]
pub struct CalibrationPanel {
    pub pairs: usize,
    pub exact_pct: f64,
    pub adjacent_pct: f64,
    pub boundary_precision: f64,
    pub boundary_recall: f64,
    pub kappa: f64,
    pub trustworthy: bool,
}

/// Average of boundary precision/recall at/above which the judge is trusted.
pub const TRUST_GATE: f64 = 0.6;

/// Fact-layer ablation outcome (Architect finding #4: distinguish "no lift" from
/// "not exercised").
#[derive(Debug, Clone)]
pub struct AblationReport {
    pub queries_touching_fact: usize,
    pub total_queries: usize,
    pub ndcg_lift_vs_ablation: f64,
    pub inconclusive: bool,
}

/// The full eval result.
#[derive(Debug, Clone)]
pub struct EvalReport {
    pub k: u32,
    pub judge_model: String,
    pub total_queries: usize,
    pub modes: Vec<ModeReport>,
    /// `graph-hybrid` minus `hybrid`.
    pub lift_ndcg: f64,
    pub lift_recall: f64,
    pub lift_mrr: f64,
    pub truncated_judgments: usize,
    pub total_judgments: usize,
    /// Pairs whose judge call failed: skipped, never scored.
    pub judge_errors: usize,
    pub ablation: AblationReport,
    pub calibration: Option<CalibrationPanel>,
}

impl EvalReport {
    /// Plain-text report.
    pub fn render(&self) -> String {
        let mut o = String::new();
        o.push_str(&format!(
            "relevance eval @ K={}  ({} queries, judge-model={})\n\n",
            self.k,
            self.total_queries,
            if self.judge_model.is_empty() { "<fabric default>" } else { &self.judge_model },
        ));

        o.push_str(&format!(
            "{:<22} {:>8} {:>10} {:>8}   {:>14} {:>9} {:>9}   {}\n",
            "mode", "nDCG@K", "Recall@K", "MRR", "n(ndcg/rec/mrr)", "p50 ms", "p95 ms", "rerank"
        ));
        o.push_str(&format!("{}\n", "-".repeat(100)));
        for m in &self.modes {
            let (p50, p95) = match m.latency.n {
                0 => ("-".to_string(), "-".to_string()),
                _ => (format!("{:.1}", m.latency.p50_ms), format!("{:.1}", m.latency.p95_ms)),
            };
            o.push_str(&format!(
                "{:<22} {:>8.4} {:>10.4} {:>8.4}   {:>4}/{:>3}/{:>3}   {:>9} {:>9}   {}\n",
                m.mode,
                m.means.ndcg,
                m.means.recall,
                m.means.mrr,
                m.means.n_ndcg,
                m.means.n_recall,
                m.means.n_mrr,
                p50,
                p95,
                m.rerank.map_or_else(|| "-".to_string(), |t| t.cell()),
            ));
        }
        for m in &self.modes {
            if let Some(t) = m.rerank
                && t.off != t.total()
                && t.ran != t.total()
            {
                o.push_str(&format!(
                    "\nWARNING: {} has rerank enabled but it did NOT run on {}/{} queries \
                     ({} latched off, {} failed open, {} off); its scores are NOT a rerank-on result\n",
                    m.mode,
                    t.total() - t.ran,
                    t.total(),
                    t.disabled,
                    t.failed_open,
                    t.off,
                ));
            }
        }

        o.push_str(&format!(
            "\nLIFT graph-hybrid vs hybrid:  nDCG {:+.4}   Recall {:+.4}   MRR {:+.4}\n",
            self.lift_ndcg, self.lift_recall, self.lift_mrr,
        ));

        o.push_str("\nfact-layer ablation (graph-hybrid vs graph-hybrid-no-fact):\n");
        if self.ablation.inconclusive {
            o.push_str(&format!(
                "  INCONCLUSIVE - fact layer not exercised ({}/{} queries touched a fact edge)\n",
                self.ablation.queries_touching_fact, self.ablation.total_queries,
            ));
        } else {
            o.push_str(&format!(
                "  nDCG lift from fact edges: {:+.4}  ({}/{} queries touched a fact edge)\n",
                self.ablation.ndcg_lift_vs_ablation, self.ablation.queries_touching_fact, self.ablation.total_queries,
            ));
        }

        o.push_str(&format!(
            "\njudgments: {} total, {} from truncated bodies (low-confidence), {} judge errors (pairs skipped)\n",
            self.total_judgments, self.truncated_judgments, self.judge_errors,
        ));

        match &self.calibration {
            Some(c) => {
                o.push_str(&format!(
                    "\ncalibration ({} hand-labeled pairs): exact {:.0}%  adjacent {:.0}%  \
                     boundary P/R {:.2}/{:.2}  kappa {:.2}  -> {}\n",
                    c.pairs,
                    c.exact_pct * 100.0,
                    c.adjacent_pct * 100.0,
                    c.boundary_precision,
                    c.boundary_recall,
                    c.kappa,
                    if c.trustworthy { "TRUSTWORTHY" } else { "LOW-CONFIDENCE (judge unvalidated)" },
                ));
            }
            None => {
                o.push_str(
                    "\ncalibration: UNCALIBRATED - no hand labels; judge unvalidated. Run --emit-calibration.\n",
                );
            }
        }
        o
    }
}

#[cfg(test)]
mod tests;
