//! The distillation eval report: per-kind axis means + composite, a judgment
//! summary (total / new / truncated / fallback), and the judge calibration
//! panel — plus a plain-text renderer for `sb` to print.

use crate::eval::calc;
use crate::eval::judge::{AxisScores, HIT_THRESHOLD};

/// Average of judge-vs-human boundary precision/recall at/above which the judge
/// is trusted (identical gate to the oracle eval).
pub const TRUST_GATE: f64 = 0.6;

/// Per-kind (or overall) axis means and composite.
#[derive(Debug, Clone, PartialEq)]
pub struct KindReport {
    pub kind: String,
    pub n: usize,
    pub claim_coverage: f64,
    pub anchor_validity: f64,
    pub summary_faithfulness: f64,
    pub composite: f64,
}

impl KindReport {
    /// Aggregate a set of per-fixture axis scores into means + composite.
    pub fn aggregate(kind: &str, scores: &[AxisScores]) -> Self {
        let n = scores.len();
        if n == 0 {
            return Self {
                kind: kind.to_string(),
                n: 0,
                claim_coverage: 0.0,
                anchor_validity: 0.0,
                summary_faithfulness: 0.0,
                composite: 0.0,
            };
        }
        let denom = n as f64;
        let cc = scores.iter().map(|s| s.claim_coverage as f64).sum::<f64>() / denom;
        let av = scores.iter().map(|s| s.anchor_validity as f64).sum::<f64>() / denom;
        let sf = scores.iter().map(|s| s.summary_faithfulness as f64).sum::<f64>() / denom;
        let composite = scores.iter().map(|s| s.composite()).sum::<f64>() / denom;
        Self {
            kind: kind.to_string(),
            n,
            claim_coverage: cc,
            anchor_validity: av,
            summary_faithfulness: sf,
            composite,
        }
    }
}

/// Judge-vs-human calibration panel (mirrors the oracle eval: report exact +
/// adjacent agreement and the hit-boundary precision/recall, gate on the mean
/// of P/R rather than kappa alone, which the class-imbalanced pool deflates).
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

/// Build the calibration panel from graded `(human, judge)` axis pairs.
/// `None` when there are no hand labels (an uncalibrated run).
pub fn calibration_panel(pairs: &[(u8, u8)]) -> Option<CalibrationPanel> {
    if pairs.is_empty() {
        return None;
    }
    let n = pairs.len() as f64;
    let exact = pairs.iter().filter(|(h, j)| h == j).count() as f64 / n;
    let adjacent = pairs.iter().filter(|(h, j)| h.abs_diff(*j) <= 1).count() as f64 / n;
    let (precision, recall) = calc::boundary_precision_recall(pairs, HIT_THRESHOLD);
    let kappa = calc::cohens_kappa(pairs);
    Some(CalibrationPanel {
        pairs: pairs.len(),
        exact_pct: exact,
        adjacent_pct: adjacent,
        boundary_precision: precision,
        boundary_recall: recall,
        kappa,
        trustworthy: (precision + recall) / 2.0 >= TRUST_GATE,
    })
}

/// The full distillation eval result.
#[derive(Debug, Clone)]
pub struct EvalReport {
    pub judge_model: String,
    pub total_fixtures: usize,
    /// Fixtures scored this run (== `total_fixtures` unless a judge call failed).
    pub total_judgments: usize,
    /// Cache misses this run: 0 on a cache-hit-stable re-run.
    pub new_judgments: usize,
    /// Judgments whose source was truncated to the judge budget (low-confidence
    /// coverage — the judge did not see the whole source).
    pub truncated_judgments: usize,
    /// Fixtures whose distilled artifact carried a `fallback_reason`.
    pub fallback_fixtures: usize,
    pub kinds: Vec<KindReport>,
    pub overall: KindReport,
    pub calibration: Option<CalibrationPanel>,
}

impl EvalReport {
    /// Plain-text report.
    pub fn render(&self) -> String {
        let mut o = String::new();
        o.push_str(&format!(
            "distillation eval  ({} fixtures, judge-model={})\n\n",
            self.total_fixtures,
            if self.judge_model.is_empty() { "<fabric default>" } else { &self.judge_model },
        ));

        o.push_str(&format!(
            "{:<12} {:>4} {:>10} {:>10} {:>10} {:>10}\n",
            "kind", "n", "coverage", "anchor", "summary", "composite"
        ));
        o.push_str(&format!("{}\n", "-".repeat(60)));
        for k in &self.kinds {
            o.push_str(&format!(
                "{:<12} {:>4} {:>10.3} {:>10.3} {:>10.3} {:>10.3}\n",
                k.kind, k.n, k.claim_coverage, k.anchor_validity, k.summary_faithfulness, k.composite,
            ));
        }
        o.push_str(&format!("{}\n", "-".repeat(60)));
        o.push_str(&format!(
            "{:<12} {:>4} {:>10.3} {:>10.3} {:>10.3} {:>10.3}\n",
            self.overall.kind,
            self.overall.n,
            self.overall.claim_coverage,
            self.overall.anchor_validity,
            self.overall.summary_faithfulness,
            self.overall.composite,
        ));

        o.push_str(&format!(
            "\njudgments: {} scored, {} new (cache misses), {} from truncated sources, {} fixtures on a distill fallback\n",
            self.total_judgments, self.new_judgments, self.truncated_judgments, self.fallback_fixtures,
        ));

        match &self.calibration {
            Some(c) => {
                o.push_str(&format!(
                    "\ncalibration ({} hand-labeled axis pairs): exact {:.0}%  adjacent {:.0}%  \
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
