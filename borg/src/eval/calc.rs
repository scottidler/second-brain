//! Pure judge-vs-human calibration math, plus the deterministic (zero-judge)
//! distillation-quality metrics (2026-07-07 distillation-output-restore,
//! Phase 7). All functions here are pure: no I/O, no LLM calls, no cache -
//! unit-testable in isolation, unlike the judge axes in `judge.rs` which need
//! a live (or mocked) `DistillationJudge`. Mirrors the oracle eval's
//! calibration statistics (`oracle/src/eval/calc.rs`), operating on graded
//! `(human, judge)` pairs on the `0..=MAX_SCORE` axis scale.

use crate::config::MAX_NOTE_BYTES;
use crate::eval::judge::MAX_SCORE;
use vault::distilled::Enumeration;

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

/// Listicle-survival: does the fixture's enumeration keep every item the
/// source declared, all N of them? Deterministic, zero judge calls - a
/// straight comparison of `items.len()` against `declared_count`.
///
/// Scores `0.0` when there is no enumeration at all (`None`) or the
/// enumeration carries no `declared_count` - this is deliberate, not a
/// degenerate "not applicable" case: a video whose source declared a listicle
/// ("Top N X") but whose distilled artifact carries no enumeration at all is
/// the exact regression this metric exists to catch (the pre-restore
/// pipeline dropped enumeration entirely). Otherwise `items.len() /
/// declared_count`, clamped to `1.0` - an over-count is not the failure mode
/// this design cares about; an under-count (a shortfall) earns partial
/// credit proportional to how much of the list survived.
pub fn listicle_survival(enumeration: Option<&Enumeration>) -> f64 {
    let Some(enumeration) = enumeration else {
        return 0.0;
    };
    let Some(declared) = enumeration.declared_count else {
        return 0.0;
    };
    if declared == 0 {
        return 0.0;
    }
    (enumeration.items.len() as f64 / declared as f64).min(1.0)
}

/// Note-size: does the fixture's rendered body stay under the publish-path
/// hard ceiling (`config::MAX_NOTE_BYTES`, Phase 3)? Deterministic - the
/// caller renders the fixture's `Distilled` via `distillers::render` and
/// passes the resulting `body_markdown` byte length; this function only
/// applies the same `<` boundary `pipeline::note_size_gate` enforces at
/// publish, so the eval metric and the live gate can never drift apart.
pub fn note_size_within_ceiling(rendered_bytes: usize) -> bool {
    rendered_bytes < MAX_NOTE_BYTES
}

#[cfg(test)]
mod tests;
