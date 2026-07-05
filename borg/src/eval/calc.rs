//! Pure judge-vs-human calibration math. Kept separate from the orchestration
//! in `eval.rs` so it is unit-testable without a cache or an LLM. Mirrors the
//! oracle eval's calibration statistics (`oracle/src/eval/calc.rs`), operating
//! on graded `(human, judge)` pairs on the `0..=MAX_SCORE` axis scale.

use crate::eval::judge::MAX_SCORE;

/// Cohen's kappa over graded `(human, judge)` pairs on the `0..=MAX_SCORE`
/// scale. Returns 0.0 for fewer than 2 pairs or when agreement equals chance.
pub fn cohens_kappa(pairs: &[(u8, u8)]) -> f64 {
    if pairs.len() < 2 {
        return 0.0;
    }
    let n = pairs.len() as f64;
    let cats = (MAX_SCORE as usize) + 1;
    let po = pairs.iter().filter(|(h, j)| h == j).count() as f64 / n;
    let mut human = vec![0.0; cats];
    let mut judge = vec![0.0; cats];
    for (h, j) in pairs {
        human[(*h).min(MAX_SCORE) as usize] += 1.0;
        judge[(*j).min(MAX_SCORE) as usize] += 1.0;
    }
    let pe: f64 = (0..cats).map(|c| (human[c] / n) * (judge[c] / n)).sum();
    if (1.0 - pe).abs() < 1e-12 {
        return 0.0;
    }
    (po - pe) / (1.0 - pe)
}

/// Judge-vs-human agreement at the `score >= threshold` hit boundary. Returns
/// `(precision, recall)`: precision = of the pairs the judge called a hit, the
/// fraction the human agrees on; recall = of the human's hits, the fraction the
/// judge caught. A degenerate denominator yields `1.0` (vacuously satisfied).
pub fn boundary_precision_recall(pairs: &[(u8, u8)], threshold: u8) -> (f64, f64) {
    let mut tp = 0usize;
    let mut judge_pos = 0usize;
    let mut human_pos = 0usize;
    for (h, j) in pairs {
        let hp = *h >= threshold;
        let jp = *j >= threshold;
        if jp {
            judge_pos += 1;
        }
        if hp {
            human_pos += 1;
        }
        if hp && jp {
            tp += 1;
        }
    }
    let precision = if judge_pos == 0 { 1.0 } else { tp as f64 / judge_pos as f64 };
    let recall = if human_pos == 0 { 1.0 } else { tp as f64 / human_pos as f64 };
    (precision, recall)
}

#[cfg(test)]
mod tests;
