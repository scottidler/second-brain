//! The distillation judge: grades how faithfully a distilled note represents
//! its source on three axes (0-3 each).
//!
//! Injected via the [`DistillationJudge`] trait so the eval pipeline runs
//! against a deterministic [`MockJudge`] in tests and the LLM-backed
//! [`FabricJudge`] in production. The judge receives ONLY the kind, the source
//! text, and the distilled note — never the extractor, model, or any pipeline
//! metadata — so the score reflects the artifact, not its provenance.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use eyre::{Result, bail};
use regex::Regex;

/// Maximum per-axis score. `0` = worst, `MAX_SCORE` = perfect.
pub const MAX_SCORE: u8 = 3;
/// Per-axis score at/above which the judge is treated as calling that axis a
/// "hit" for calibration boundary precision/recall (mirrors the oracle eval).
pub const HIT_THRESHOLD: u8 = 2;

/// The three rubric axes, each on the `0..=MAX_SCORE` scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AxisScores {
    /// Fraction of the source's key claims the note represents.
    pub claim_coverage: u8,
    /// Whether claim anchors are valid/consistent with the source.
    pub anchor_validity: u8,
    /// Whether the summary faithfully represents the source (no hallucination).
    pub summary_faithfulness: u8,
}

impl AxisScores {
    /// Clamp every axis into `0..=MAX_SCORE`.
    pub fn clamped(self) -> Self {
        Self {
            claim_coverage: self.claim_coverage.min(MAX_SCORE),
            anchor_validity: self.anchor_validity.min(MAX_SCORE),
            summary_faithfulness: self.summary_faithfulness.min(MAX_SCORE),
        }
    }

    /// Composite score = the mean of the three axes.
    pub fn composite(self) -> f64 {
        (self.claim_coverage as f64 + self.anchor_validity as f64 + self.summary_faithfulness as f64) / 3.0
    }
}

/// Grades a distilled note against its source on the three axes.
pub trait DistillationJudge {
    /// Return the graded axes for `note` (a distillation) against `source`.
    /// `kind` lets the judge apply kind-appropriate anchor expectations.
    fn judge(&self, kind: &str, source: &str, note: &str) -> Result<AxisScores>;
}

/// Deterministic, fixture-driven judge for tests. Returns per-kind scores with a
/// fallback default, and counts calls so the cache-stability test can assert
/// that a re-run makes zero judge calls.
#[derive(Debug)]
pub struct MockJudge {
    default: AxisScores,
    by_kind: HashMap<String, AxisScores>,
    calls: AtomicUsize,
}

impl MockJudge {
    /// A judge returning `default` for every kind unless overridden.
    pub fn new(default: AxisScores) -> Self {
        Self {
            default: default.clamped(),
            by_kind: HashMap::new(),
            calls: AtomicUsize::new(0),
        }
    }

    /// Override the scores for a specific kind (builder style).
    pub fn with(mut self, kind: &str, scores: AxisScores) -> Self {
        self.by_kind.insert(kind.to_string(), scores.clamped());
        self
    }

    /// Number of `judge` calls made so far (cache misses in an eval run).
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl DistillationJudge for MockJudge {
    fn judge(&self, kind: &str, _source: &str, _note: &str) -> Result<AxisScores> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.by_kind.get(kind).copied().unwrap_or(self.default))
    }
}

/// Default char budget for the source text sent to the judge.
const DEFAULT_JUDGE_MAX_CHARS: usize = 24_000;
/// Default per-call fabric timeout for the judge.
const DEFAULT_JUDGE_TIMEOUT_SECS: u64 = 90;

/// Production judge: runs the `judge-distillation` Fabric pattern over the
/// `(kind, source, note)` text and parses three axis scores from the YAML reply.
#[derive(Debug, Clone)]
pub struct FabricJudge {
    /// Fabric binary name (resolved on `PATH`).
    pub binary: String,
    /// Model name; empty = fabric's default model.
    pub model: String,
    /// Fabric pattern name (resolved under `~/.config/sb/patterns/`).
    pub pattern: String,
    /// Truncation budget (chars) for the text sent to the judge.
    pub max_chars: usize,
    /// Per-call fabric timeout.
    pub timeout_secs: u64,
}

impl FabricJudge {
    /// A judge using the `judge-distillation` pattern and the given model
    /// (empty string = fabric's default model).
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            binary: "fabric".to_string(),
            model: model.into(),
            pattern: "judge-distillation".to_string(),
            max_chars: DEFAULT_JUDGE_MAX_CHARS,
            timeout_secs: DEFAULT_JUDGE_TIMEOUT_SECS,
        }
    }
}

impl DistillationJudge for FabricJudge {
    fn judge(&self, kind: &str, source: &str, note: &str) -> Result<AxisScores> {
        log::debug!(
            "FabricJudge::judge: kind={} pattern={} model={} source_len={} note_len={}",
            kind,
            self.pattern,
            self.model,
            source.len(),
            note.len()
        );
        let input = format!("# KIND\n{kind}\n\n# SOURCE\n{source}\n\n# DISTILLED NOTE\n{note}\n");
        // Eval-only path: no configured credential var is threaded here, so pass
        // "" and let fabric fall back to its own .env (prior behavior). The
        // daemon/harvest hot path routes through the borg/cortex FabricConfig
        // wrapper, which carries the mirrored llm.api-key.
        let reply = vault::fabric::run_pattern(
            &self.pattern,
            &input,
            &self.binary,
            "",
            &self.model,
            self.max_chars,
            self.timeout_secs,
        )?;
        parse_axis_scores(&reply)
    }
}

/// Parse the three axis scores from a judge reply. Lenient by construction: the
/// model is asked for a YAML mapping, but real replies drift (code fences,
/// prose preambles), so each axis is extracted by name with a regex and clamped
/// to `0..=MAX_SCORE`. Errors when any axis is missing — the caller treats a
/// missing axis as a failed judgment rather than silently scoring 0.
pub fn parse_axis_scores(reply: &str) -> Result<AxisScores> {
    let claim_coverage = extract_axis(reply, "claim.coverage")?;
    let anchor_validity = extract_axis(reply, "anchor.validity")?;
    let summary_faithfulness = extract_axis(reply, "summary.faithfulness")?;
    Ok(AxisScores {
        claim_coverage,
        anchor_validity,
        summary_faithfulness,
    }
    .clamped())
}

/// Extract one axis score by name. `axis_word_boundary` is a regex fragment where
/// `.` matches the hyphen/space/underscore separator between the two words.
fn extract_axis(reply: &str, axis: &str) -> Result<u8> {
    // e.g. `claim.coverage` -> `(?i)claim[-_ ]coverage\s*[:=]\s*([0-9])`
    let axis_re = axis.replace('.', "[-_ ]");
    let pattern = format!(r"(?i){axis_re}\s*[:=]\s*([0-9]+)");
    let re = Regex::new(&pattern).expect("static axis regex compiles");
    match re.captures(reply).and_then(|c| c.get(1)) {
        Some(m) => {
            let n: u32 = m.as_str().parse().unwrap_or(0);
            Ok((n.min(MAX_SCORE as u32)) as u8)
        }
        None => bail!(
            "judge reply missing axis '{}': {:?}",
            axis,
            reply.chars().take(120).collect::<String>()
        ),
    }
}

#[cfg(test)]
mod tests;
