//! The paired A/B comparison behind `sb oracle eval --compare-db/--compare-config`
//! (design: `docs/design/2026-10-29-retrieval-quality-from-rag-scour.md`,
//! Decision rule).
//!
//! Every target runs on both arms. Per (query, target) the pool is the union of
//! that target's arm-A and arm-B top-K, and IDCG comes from that pool only, so
//! adding or removing another target can never move a target's delta. Queries
//! split deterministically into tune and confirm on `fnv1a(id) % 2`. Each
//! target and split gets a paired mean delta (B - A) with a percentile
//! bootstrap 95% CI, the MDD diagnostic, and a verdict from the pre-registered
//! rule. Pure functions over ranked lists and judgments: no index, no LLM.

use std::collections::HashSet;

use crate::eval::cache::fnv1a;
use crate::eval::metrics::{self, Judgments};
use crate::eval::report::{Latency, RerankTally};

/// Bootstrap resamples per CI (pre-registered).
pub const BOOTSTRAP_RESAMPLES: usize = 10_000;
/// Fixed bootstrap seed (pre-registered), so a rerun reproduces the CI exactly.
pub const BOOTSTRAP_SEED: u64 = 0x2026_1029;
/// `MDD = MDD_FACTOR * sd / sqrt(n)`: two-sided alpha 0.05 plus 80% power.
const MDD_FACTOR: f64 = 2.8;

/// Which half of the query set a query belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Split {
    /// Settings are picked here.
    Tune,
    /// Ship decisions are read here.
    Confirm,
}

impl Split {
    /// Both splits, in report order.
    pub const ALL: [Split; 2] = [Split::Tune, Split::Confirm];

    /// `fnv1a(id) % 2`: even is tune, odd is confirm. Keyed on the id, so a
    /// query keeps its split as the set grows.
    pub fn of(query_id: &str) -> Self {
        if fnv1a(query_id).is_multiple_of(2) { Split::Tune } else { Split::Confirm }
    }

    pub fn label(self) -> &'static str {
        match self {
            Split::Tune => "tune",
            Split::Confirm => "confirm",
        }
    }
}

/// The pre-registered outcome for one target and split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// CI lower bound > 0 and, when a budget is set, arm B's p95 within it.
    Ship,
    /// Enough data, but the rule is not met.
    Hold,
    /// n <= 1 or zero variance: no CI or MDD can be trusted. Blocks shipping.
    Insufficient,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Ship => "ship",
            Verdict::Hold => "hold",
            Verdict::Insufficient => "insufficient",
        }
    }
}

/// One query's paired nDCG for one target.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PairedScore {
    pub ndcg_a: f64,
    pub ndcg_b: f64,
}

/// nDCG@k of `list_a` and `list_b` against the paired pool, the union of both
/// lists' top `k`. Judgments outside that pool are ignored, so IDCG depends on
/// this target's two lists only. `None` when the pool holds no relevant note
/// (the query cannot discriminate the arms and is excluded from n).
pub fn paired_ndcg(list_a: &[String], list_b: &[String], judgments: &Judgments, k: usize) -> Option<PairedScore> {
    let pool: HashSet<&str> = list_a
        .iter()
        .take(k)
        .chain(list_b.iter().take(k))
        .map(String::as_str)
        .collect();
    let pooled: Judgments = judgments
        .iter()
        .filter(|(path, _)| pool.contains(path.as_str()))
        .map(|(path, score)| (path.clone(), *score))
        .collect();
    let ndcg_a = metrics::ndcg_at_k(list_a, &pooled, k)?;
    let ndcg_b = metrics::ndcg_at_k(list_b, &pooled, k)?;
    Some(PairedScore { ndcg_a, ndcg_b })
}

/// SplitMix64: a fixed, dependency-free PRNG so the bootstrap is reproducible
/// across toolchains and crate versions.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform index in `0..n` (multiply-shift; `n` is a query count, so the
    /// bias is far below anything a CI could show).
    fn index(&mut self, n: usize) -> usize {
        ((self.next_u64() as u128 * n as u128) >> 64) as usize
    }
}

/// Percentile bootstrap 95% CI of the mean of `deltas`: `resamples` resamples
/// with replacement from a PRNG seeded with `seed`, then the 2.5th and 97.5th
/// percentiles of the resampled means. `(0.0, 0.0)` for an empty input.
pub fn bootstrap_ci(deltas: &[f64], resamples: usize, seed: u64) -> (f64, f64) {
    if deltas.is_empty() || resamples == 0 {
        return (0.0, 0.0);
    }
    let n = deltas.len();
    let mut rng = SplitMix64(seed);
    let mut means: Vec<f64> = (0..resamples)
        .map(|_| (0..n).map(|_| deltas[rng.index(n)]).sum::<f64>() / n as f64)
        .collect();
    means.sort_by(f64::total_cmp);
    let lower = (0.025 * resamples as f64).floor() as usize;
    let upper = ((0.975 * resamples as f64).ceil() as usize).clamp(1, resamples) - 1;
    (means[lower.min(resamples - 1)], means[upper])
}

/// Sample standard deviation (n - 1). `None` for fewer than two samples.
fn sample_sd(xs: &[f64]) -> Option<f64> {
    if xs.len() < 2 {
        return None;
    }
    let mean = xs.iter().sum::<f64>() / xs.len() as f64;
    let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (xs.len() - 1) as f64;
    Some(var.sqrt())
}

/// Minimum detectable difference, `2.8 * sd / sqrt(n)`, a planning diagnostic
/// only. `None` (printed `insufficient`) when n <= 1 or the deltas have zero
/// variance.
pub fn mdd(deltas: &[f64]) -> Option<f64> {
    // Zero variance is tested on the samples, not on the computed sd: the mean
    // of identical non-dyadic values (0.2, 0.2, 0.2) carries rounding error, so
    // the sd comes out as ~1e-17, not 0.
    if deltas.iter().all(|d| *d == deltas[0]) {
        return None;
    }
    let sd = sample_sd(deltas)?;
    Some(MDD_FACTOR * sd / (deltas.len() as f64).sqrt())
}

/// The pre-registered rule: `insufficient` when n <= 1 or zero variance; else
/// `ship` iff the CI lower bound is > 0 and, when `budget_ms` is set, arm B's
/// p95 is within it; else `hold`.
pub fn verdict(deltas: &[f64], ci: (f64, f64), p95_b_ms: f64, budget_ms: Option<u64>) -> Verdict {
    if mdd(deltas).is_none() {
        return Verdict::Insufficient;
    }
    let within_budget = budget_ms.is_none_or(|b| p95_b_ms <= b as f64);
    if ci.0 > 0.0 && within_budget { Verdict::Ship } else { Verdict::Hold }
}

/// One row of the paired table: a target on one split.
#[derive(Debug, Clone, PartialEq)]
pub struct PairedRow {
    pub target: String,
    pub split: Split,
    /// Mean nDCG@K of each arm over the n paired queries.
    pub ndcg_a: f64,
    pub ndcg_b: f64,
    /// Mean of the per-query `b - a` deltas.
    pub delta: f64,
    pub ci: (f64, f64),
    pub n: usize,
    /// `None` = insufficient.
    pub mdd: Option<f64>,
    /// Retrieval wall-clock of each arm over every query in the split.
    pub latency_a: Latency,
    pub latency_b: Latency,
    pub verdict: Verdict,
}

/// Summarize one target on one split from its paired scores and both arms'
/// latency samples.
pub fn summarize(
    target: &str,
    split: Split,
    scores: &[PairedScore],
    latency_a: Vec<f64>,
    latency_b: Vec<f64>,
    budget_ms: Option<u64>,
) -> PairedRow {
    let n = scores.len();
    let deltas: Vec<f64> = scores.iter().map(|s| s.ndcg_b - s.ndcg_a).collect();
    let mean = |f: fn(&PairedScore) -> f64| {
        if n == 0 { 0.0 } else { scores.iter().map(f).sum::<f64>() / n as f64 }
    };
    let ci = bootstrap_ci(&deltas, BOOTSTRAP_RESAMPLES, BOOTSTRAP_SEED);
    let latency_b = Latency::from_samples(latency_b);
    PairedRow {
        target: target.to_string(),
        split,
        ndcg_a: mean(|s| s.ndcg_a),
        ndcg_b: mean(|s| s.ndcg_b),
        delta: if n == 0 { 0.0 } else { deltas.iter().sum::<f64>() / n as f64 },
        ci,
        n,
        mdd: mdd(&deltas),
        latency_a: Latency::from_samples(latency_a),
        verdict: verdict(&deltas, ci, latency_b.p95_ms, budget_ms),
        latency_b,
    }
}

/// The paired comparison: one row per target and split, plus what defined
/// arm B and whether latency gated the verdict.
#[derive(Debug, Clone, PartialEq)]
pub struct PairedReport {
    /// Arm B's DB, when it differs from arm A's.
    pub compare_db: Option<String>,
    /// Arm B's `oracle.yml`, when it differs from arm A's.
    pub compare_config: Option<String>,
    /// p95 budget for arm B; `None` = latency reported, not gated.
    pub latency_budget_ms: Option<u64>,
    /// What each arm's `configured` rerank stage did, so a latched-off stage
    /// on arm B can never read as a rerank-on result.
    pub rerank_a: RerankTally,
    pub rerank_b: RerankTally,
    pub rows: Vec<PairedRow>,
}

impl PairedReport {
    /// Plain-text table, one line per target and split.
    pub fn render(&self) -> String {
        let mut o = String::new();
        o.push_str(&format!(
            "\npaired A/B (B - A, per-target A∪B pool; bootstrap {BOOTSTRAP_RESAMPLES} resamples, seed {BOOTSTRAP_SEED:#x})\n",
        ));
        o.push_str(&format!(
            "  arm B: db={}  config={}\n",
            self.compare_db.as_deref().unwrap_or("<arm A>"),
            self.compare_config.as_deref().unwrap_or("<arm A>"),
        ));
        o.push_str(&match self.latency_budget_ms {
            Some(b) => format!("  latency: ship also needs arm B p95 <= {b} ms\n"),
            None => "  latency: reported, NOT gated (no --latency-budget-ms)\n".to_string(),
        });
        o.push_str(&format!(
            "  configured rerank: arm A {}, arm B {}\n",
            self.rerank_a.cell(),
            self.rerank_b.cell()
        ));
        for (arm, t) in [("A", &self.rerank_a), ("B", &self.rerank_b)] {
            if t.off != t.total() && t.ran != t.total() {
                o.push_str(&format!(
                    "  WARNING: arm {arm} has rerank enabled but it did NOT run on {}/{} queries; \
                     its configured rows are NOT a rerank-on result\n",
                    t.total() - t.ran,
                    t.total()
                ));
            }
        }
        o.push_str("  the ship decision reads the confirm rows; tune rows are for picking settings\n\n");
        o.push_str(&format!(
            "{:<22} {:<8} {:>7} {:>7} {:>8} {:>19} {:>4} {:>12} {:>9} {:>9}   {}\n",
            "target", "split", "nDCG A", "nDCG B", "delta", "95% CI", "n", "MDD", "p95 A", "p95 B", "verdict"
        ));
        o.push_str(&format!("{}\n", "-".repeat(126)));
        for r in &self.rows {
            let ms = |l: &Latency| if l.n == 0 { "-".to_string() } else { format!("{:.1}", l.p95_ms) };
            o.push_str(&format!(
                "{:<22} {:<8} {:>7.4} {:>7.4} {:>+8.4} {:>19} {:>4} {:>12} {:>9} {:>9}   {}\n",
                r.target,
                r.split.label(),
                r.ndcg_a,
                r.ndcg_b,
                r.delta,
                format!("[{:+.4}, {:+.4}]", r.ci.0, r.ci.1),
                r.n,
                r.mdd.map_or_else(|| "insufficient".to_string(), |m| format!("{m:.4}")),
                ms(&r.latency_a),
                ms(&r.latency_b),
                r.verdict.label(),
            ));
        }
        o
    }
}

#[cfg(test)]
mod tests;
