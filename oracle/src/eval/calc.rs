//! Pure helpers for the eval pipeline: judged-text preparation and
//! judge-vs-human calibration math. Kept separate from the orchestration in
//! `eval.rs` so they are unit-testable without a DB or an LLM.

use crate::eval::judge::MAX_SCORE;

/// Render Obsidian wikilink markup to the text a reader sees, so the judge
/// reads prose, not link syntax: the alias when there is one, else the target
/// (`[[a#h]]` -> `a > h`, as Obsidian shows it; `[[#h]]` -> `h`). Links inside
/// code stay literal, as Obsidian renders them; other text passes through.
pub fn flatten_wikilinks(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    for link in vault::wikilink::parse(s) {
        out.push_str(&s[last..link.span.start]);
        let fragment = link.heading.or(link.block);
        match (link.alias, fragment) {
            (Some(alias), _) => out.push_str(alias),
            (None, Some(fragment)) if link.target.is_empty() => out.push_str(fragment),
            (None, Some(fragment)) => {
                out.push_str(link.target);
                out.push_str(" > ");
                out.push_str(fragment);
            }
            (None, None) => out.push_str(link.target),
        }
        last = link.span.end;
    }
    out.push_str(&s[last..]);
    out
}

/// Prepare the note text the judge sees: the distilled `summary`, then a blank
/// line, then the note's `claims` one per `- ` line, capped at `max_chars` with
/// the summary first. Claims are cut at a claim boundary: a claim that does not
/// fit whole is dropped along with every claim after it. A summary longer than
/// the budget is cut at `max_chars` and carries no claims. The body is the
/// fallback only when there is no summary. Wikilinks are flattened throughout.
///
/// The text is a function of the note alone, never of the target or arm that
/// surfaced it, so one note scores the same in every arm of a paired eval.
/// Returns `(text, truncated)` where `truncated` is true only when the *body
/// fallback* had to be cut (a low-confidence judgment, since the judge may not
/// have seen a deep match); a summary is bounded and dropped trailing claims
/// leave the summary intact, so neither counts as a truncation hazard.
pub fn prepare_note_text(summary: &str, claims: &[String], body: &str, max_chars: usize) -> (String, bool) {
    if !summary.trim().is_empty() {
        let flat = flatten_wikilinks(summary);
        let mut text: String = flat.chars().take(max_chars).collect();
        let mut used = text.chars().count();
        let mut separator = CLAIMS_SEPARATOR;
        for claim in claims
            .iter()
            .map(|c| flatten_wikilinks(c.trim()))
            .filter(|c| !c.is_empty())
        {
            let line = format!("{separator}- {claim}");
            let len = line.chars().count();
            if used + len > max_chars {
                break;
            }
            text.push_str(&line);
            used += len;
            separator = "\n";
        }
        return (text, false);
    }
    let flat = flatten_wikilinks(body);
    let truncated = flat.chars().count() > max_chars;
    let capped: String = flat.chars().take(max_chars).collect();
    (capped, truncated)
}

/// Between the summary and the first claim.
const CLAIMS_SEPARATOR: &str = "\n\n";

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
