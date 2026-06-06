//! The eval report: per-mode metrics, lift over baselines, calibration panel,
//! and the fact-layer ablation — plus a plain-text renderer for `sb` to print.

use crate::eval::metrics::MetricMeans;

/// Metrics for one search mode (or the ablation variant).
#[derive(Debug, Clone)]
pub struct ModeReport {
    pub mode: String,
    pub means: MetricMeans,
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
            "{:<22} {:>8} {:>10} {:>8}   {:>14}\n",
            "mode", "nDCG@K", "Recall@K", "MRR", "n(ndcg/rec/mrr)"
        ));
        o.push_str(&format!("{}\n", "-".repeat(66)));
        for m in &self.modes {
            o.push_str(&format!(
                "{:<22} {:>8.4} {:>10.4} {:>8.4}   {:>4}/{:>3}/{:>3}\n",
                m.mode, m.means.ndcg, m.means.recall, m.means.mrr, m.means.n_ndcg, m.means.n_recall, m.means.n_mrr,
            ));
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
            "\njudgments: {} total, {} from truncated bodies (low-confidence)\n",
            self.total_judgments, self.truncated_judgments,
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
