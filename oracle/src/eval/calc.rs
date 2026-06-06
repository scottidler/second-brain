//! Pure helpers for the eval pipeline: judged-text preparation and
//! judge-vs-human calibration math. Kept separate from the orchestration in
//! `eval.rs` so they are unit-testable without a DB or an LLM.

use crate::eval::judge::MAX_SCORE;

/// Render Obsidian wikilink markup to plain display text so the judge reads
/// prose, not link syntax: `[[target|Display]]` -> `Display`, `[[target]]` ->
/// `target`. Non-link text passes through untouched.
pub fn flatten_wikilinks(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < s.len() {
        if bytes[i] == b'['
            && i + 1 < s.len()
            && bytes[i + 1] == b'['
            && let Some(close) = s[i + 2..].find("]]")
        {
            let inner = &s[i + 2..i + 2 + close];
            let display = inner.split('|').next_back().unwrap_or(inner);
            out.push_str(display);
            i = i + 2 + close + 2;
            continue;
        }
        // push one char (handle UTF-8 boundaries)
        let ch = s[i..].chars().next().expect("char boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Prepare the note text the judge sees: prefer the bounded distilled `summary`;
/// fall back to the body only when there is no summary. Flatten wikilinks and
/// truncate to `max_chars`. Returns `(text, truncated)` where `truncated` is true
/// only when the *body fallback* had to be cut (a low-confidence judgment, since
/// the judge may not have seen a deep match).
pub fn prepare_note_text(summary: &str, body: &str, max_chars: usize) -> (String, bool) {
    if !summary.trim().is_empty() {
        // Summaries are bounded; flatten and (defensively) cap, but a summary
        // does not count as a truncation hazard.
        let flat = flatten_wikilinks(summary);
        let capped: String = flat.chars().take(max_chars).collect();
        return (capped, false);
    }
    let flat = flatten_wikilinks(body);
    let truncated = flat.chars().count() > max_chars;
    let capped: String = flat.chars().take(max_chars).collect();
    (capped, truncated)
}

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
        human[*h as usize] += 1.0;
        judge[*j as usize] += 1.0;
    }
    let pe: f64 = (0..cats).map(|c| (human[c] / n) * (judge[c] / n)).sum();
    if (1.0 - pe).abs() < 1e-12 {
        return 0.0;
    }
    (po - pe) / (1.0 - pe)
}

/// Judge-vs-human agreement at the `rel >= threshold` hit boundary (the boundary
/// the metrics actually use). Returns `(precision, recall)`: precision = of the
/// pairs the judge called a hit, the fraction the human agrees on; recall = of
/// the human's hits, the fraction the judge caught. A degenerate denominator
/// yields `1.0` (vacuously satisfied).
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
