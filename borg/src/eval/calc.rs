//! Pure judge-vs-human calibration math, plus the deterministic (zero-judge)
//! distillation-quality metrics (distillation-output-restore
//! design). All functions here are pure: no I/O, no LLM calls, no cache -
//! unit-testable in isolation, unlike the judge axes in `judge.rs` which need
//! a live (or mocked) `DistillationJudge`. Mirrors the oracle eval's
//! calibration statistics (`oracle/src/eval/calc.rs`), operating on graded
//! `(human, judge)` pairs on the `0..=MAX_SCORE` axis scale.

use crate::config::MAX_NOTE_BYTES;
use crate::eval::judge::MAX_SCORE;
use std::collections::HashSet;

use vault::distilled::{Distilled, Enumeration};

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

/// Note-size: does the fixture's rendered body stay within the publish-path
/// hard ceiling (`config::MAX_NOTE_BYTES`)? Deterministic - the
/// caller renders the fixture's `Distilled` via `distillers::render` and
/// passes the resulting `body_markdown` byte length. Delegates to the shared
/// `config::within_note_size_ceiling` predicate that `pipeline::note_size_gate`
/// also uses, so the eval metric and the live gate agree at every byte,
/// including the exact ceiling (both allow it).
pub fn note_size_within_ceiling(rendered_bytes: usize) -> bool {
    crate::config::within_note_size_ceiling(rendered_bytes, MAX_NOTE_BYTES)
}

/// Mean of the applicable per-fixture listicle-survival scores (the
/// `sb borg eval` report wiring). `None` when no fixture is applicable -
/// distinct from `Some(0.0)`, which means every applicable fixture scored a
/// genuine failure. "Applicable" is decided by the caller (report.rs): a
/// fixture whose EXPECTED `distilled.yml` declares a `declared_count` is the
/// authored assertion "this is a listicle and must survive"; fixtures without
/// one are excluded from this aggregate entirely, never folded in as 0.0.
pub fn listicle_aggregate(scores: &[f64]) -> Option<f64> {
    if scores.is_empty() {
        return None;
    }
    Some(scores.iter().sum::<f64>() / scores.len() as f64)
}

/// Word length of the shingles quote grounding matches on.
const GROUNDING_SHINGLE: usize = 3;

/// Lowercased alphanumeric words of `s`, with `H:MM`/`HH:MM:SS` timestamps
/// dropped. Smart quotes, markdown escapes, and reflowed whitespace all
/// collapse away, so only the words have to match.
pub fn grounding_words(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    for raw in s.split_whitespace() {
        let is_timestamp = {
            let parts: Vec<&str> = raw.trim_matches(|c: char| !c.is_ascii_digit()).split(':').collect();
            (2..=3).contains(&parts.len())
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.len() <= 2 && p.chars().all(|c| c.is_ascii_digit()))
        };
        if is_timestamp {
            continue;
        }
        for w in raw.split(|c: char| !c.is_alphanumeric()) {
            if !w.is_empty() {
                words.push(w.to_lowercase());
            }
        }
    }
    words
}

/// One quote piece is grounded when every 3-word shingle occurs in the source;
/// a piece under 3 words must occur as a contiguous word run.
fn piece_grounded(piece: &[String], source_words: &[String], source_shingles: &HashSet<&[String]>) -> bool {
    if piece.len() < GROUNDING_SHINGLE {
        return source_words.windows(piece.len()).any(|w| w == piece);
    }
    piece.windows(GROUNDING_SHINGLE).all(|s| source_shingles.contains(s))
}

fn shingles(words: &[String]) -> HashSet<&[String]> {
    words.windows(GROUNDING_SHINGLE).collect()
}

/// Quote grounding: of the claims carrying a verbatim `quote`, how many are
/// found in the source. A quote is grounded when every 3-word shingle of it
/// occurs in the source (a quote under 3 words must occur as a word run), and
/// an elided quote (`A ... B`) when each piece is.
/// Shingles, not one substring, because YouTube auto-captions repeat each
/// line's words in rolling windows with timestamps interleaved: an exact
/// quote never survives as one run there. Measured on 1,562 staged quotes, a
/// quote scored against the wrong source passes 0-2% (6% on short image
/// quotes), so the shingle rule does not pass paraphrase. Deterministic, zero
/// judge calls. Returns `(grounded, quoted)`; `quoted == 0` means N/A.
pub fn quote_grounding(source: &str, distilled: &Distilled) -> (usize, usize) {
    let source_words = grounding_words(source);
    let source_shingles = shingles(&source_words);
    let mut grounded = 0usize;
    let mut quoted = 0usize;
    for claim in &distilled.claims {
        let Some(quote) = claim.quote.as_deref() else {
            continue;
        };
        // An elided quote ("A ... B") is grounded piece by piece: a shingle
        // spanning the elision never occurs in the source by design.
        let pieces: Vec<Vec<String>> = quote
            .split("...")
            .flat_map(|p| p.split('…'))
            .map(grounding_words)
            .filter(|w| !w.is_empty())
            .collect();
        if pieces.is_empty() {
            continue;
        }
        quoted += 1;
        if pieces
            .iter()
            .all(|w| piece_grounded(w, &source_words, &source_shingles))
        {
            grounded += 1;
        }
    }
    (grounded, quoted)
}

/// Counts behind the `tools-grounding` metric for one note.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToolsGrounding {
    pub names_grounded: usize,
    pub names: usize,
    pub commands_grounded: usize,
    pub commands: usize,
}

impl ToolsGrounding {
    /// No tool to grade: the metric is N/A for this note.
    pub fn is_empty(&self) -> bool {
        self.names == 0 && self.commands == 0
    }
}

/// `s` with every whitespace run collapsed to one space, ends trimmed.
fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A command is grounded only when it occurs in the source as an exact,
/// case-sensitive substring once both sides have their whitespace collapsed.
/// No shingles and no lowercasing: `cargo test --release` against a source
/// saying `cargo test release` is a different command, and a reader who pastes
/// it gets a different result.
pub fn command_grounded(source: &str, command: &str) -> bool {
    command_in_collapsed_source(&collapse_whitespace(source), command)
}

fn command_in_collapsed_source(collapsed_source: &str, command: &str) -> bool {
    let command = collapse_whitespace(command);
    !command.is_empty() && collapsed_source.contains(&command)
}

/// Tools grounding: of the tools the note renders, how many names and commands
/// the source backs. A name is grounded by the quote-grounding shingle rule (a
/// name with no words is ungrounded); a command by [`command_grounded`].
/// Commands come from `distillers::tools_to_render`, so a repo's `install` line
/// counts; names come from `Distilled.tools` only, because the `Install` label
/// render gives that line is not extracted text. Deterministic, zero judge calls.
pub fn tools_grounding(source: &str, distilled: &Distilled) -> ToolsGrounding {
    let source_words = grounding_words(source);
    let source_shingles = shingles(&source_words);
    let collapsed_source = collapse_whitespace(source);
    let mut counts = ToolsGrounding::default();
    for tool in distilled.tools.iter().filter(|t| !t.name.trim().is_empty()) {
        counts.names += 1;
        let name_words = grounding_words(&tool.name);
        if !name_words.is_empty() && piece_grounded(&name_words, &source_words, &source_shingles) {
            counts.names_grounded += 1;
        }
    }
    for tool in distillers::tools_to_render(distilled) {
        let Some(command) = tool.command.as_deref().filter(|c| !c.trim().is_empty()) else {
            continue;
        };
        counts.commands += 1;
        if command_in_collapsed_source(&collapsed_source, command) {
            counts.commands_grounded += 1;
        }
    }
    counts
}

/// Placeholder artifacts: LLM filler like `[not detailed in provided content]`
/// anywhere in the summary, tldr, key ideas, or claims. Reuses the ingest
/// gate's indicator list (`quality::detect_truncation_artifacts`). Returns one
/// reason per offending field.
pub fn placeholder_artifacts(distilled: &Distilled) -> Vec<String> {
    let mut fields: Vec<(&str, &str)> = vec![("summary", distilled.summary.as_str())];
    if let Some(tldr) = distilled.tldr.as_deref() {
        fields.push(("tldr", tldr));
    }
    for idea in &distilled.key_ideas {
        fields.push(("key-idea", idea.as_str()));
    }
    for claim in &distilled.claims {
        fields.push(("claim", claim.text.as_str()));
    }
    fields
        .into_iter()
        .filter_map(|(field, text)| {
            crate::quality::detect_truncation_artifacts(text).map(|reason| format!("{field}: {reason}"))
        })
        .collect()
}

#[cfg(test)]
mod tests;
