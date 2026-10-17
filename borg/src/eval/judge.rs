//! The distillation judge: grades how faithfully a distilled note represents
//! its source on three axes (0-3 each).
//!
//! Injected via the [`DistillationJudge`] trait so the eval pipeline runs
//! against a deterministic [`MockJudge`] in tests and the LLM-backed
//! [`FabricJudge`] in production. The judge receives ONLY the kind, the source
//! text, and the distilled note — never the extractor, model, or any pipeline
//! metadata — so the score reflects the artifact, not its provenance.

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use std::process::Command;

use eyre::{Result, bail};
use regex::Regex;
use vault::fabric::ApiKeys;

/// Maximum per-axis score. `0` = worst, `MAX_SCORE` = perfect.
pub const MAX_SCORE: u8 = 3;
/// Per-axis score at/above which the judge is treated as calling that axis a
/// "hit" for calibration boundary precision/recall (mirrors the oracle eval).
pub const HIT_THRESHOLD: u8 = 2;

/// The three rubric axes, each on the `0..=MAX_SCORE` scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
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

#[cfg(test)]
/// Deterministic, fixture-driven judge for tests. Returns per-kind scores with a
/// fallback default, and counts calls so the cache-stability test can assert
/// that a re-run makes zero judge calls.
#[derive(Debug)]
pub struct MockJudge {
    default: AxisScores,
    by_kind: HashMap<String, AxisScores>,
    calls: AtomicUsize,
}

#[cfg(test)]
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

#[cfg(test)]
impl DistillationJudge for MockJudge {
    fn judge(&self, kind: &str, _source: &str, _note: &str) -> Result<AxisScores> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.by_kind.get(kind).copied().unwrap_or(self.default))
    }
}

/// Char budget for the whole judge input. `vault::fabric` keeps the head of
/// the input at this many chars, so the eval budgets the source to what the
/// note leaves ([`source_budget`]) and nothing past it is ever clipped.
pub const JUDGE_INPUT_MAX_CHARS: usize = 24_000;
/// Default per-call fabric timeout for the judge.
const DEFAULT_JUDGE_TIMEOUT_SECS: u64 = 90;

/// The `judge-distillation` input: kind, then the note, then the source.
/// The note goes before the source so fabric's head truncation can only ever
/// clip the source tail, never the note being judged.
pub fn distillation_input(kind: &str, note: &str, source: &str) -> String {
    format!("# KIND\n{kind}\n\n# DISTILLED NOTE\n{note}\n\n# SOURCE\n{source}\n")
}

/// Chars of source that fit in a `max_chars` judge input after the framing
/// and the full note: `max_chars - chars(distillation_input(kind, note, ""))`.
/// 0 when the note alone fills the budget.
pub fn source_budget(max_chars: usize, kind: &str, note: &str) -> usize {
    max_chars.saturating_sub(distillation_input(kind, note, "").chars().count())
}

/// Production judge: runs the `judge-distillation` Fabric pattern over the
/// `(kind, note, source)` text and parses three axis scores from the YAML reply.
#[derive(Debug, Clone)]
pub struct FabricJudge {
    /// Fabric binary name (resolved on `PATH`).
    pub binary: String,
    /// Model name; empty = fabric's default model.
    pub model: String,
    /// Per-vendor credential vars (`llm.api-keys`), exported under fabric's
    /// names on the child; empty leaves fabric on its own `.env`.
    pub api_keys: ApiKeys,
    /// Fabric pattern name (resolved under `~/.config/sb/patterns/`).
    pub pattern: String,
    /// Truncation budget (chars) for the text sent to the judge.
    pub max_chars: usize,
    /// Per-call fabric timeout.
    pub timeout_secs: u64,
}

impl FabricJudge {
    /// A judge using the `judge-distillation` pattern, the given model (empty
    /// string = fabric's default model) and per-vendor credential vars.
    pub fn new(model: impl Into<String>, api_keys: ApiKeys) -> Self {
        Self {
            binary: "fabric".to_string(),
            model: model.into(),
            api_keys,
            pattern: "judge-distillation".to_string(),
            max_chars: JUDGE_INPUT_MAX_CHARS,
            timeout_secs: DEFAULT_JUDGE_TIMEOUT_SECS,
        }
    }

    /// The exact fabric child this judge runs.
    pub fn command(&self) -> Command {
        vault::fabric::command(&self.binary, &self.pattern, &self.model, &self.api_keys, 0)
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
        let input = distillation_input(kind, note, source);
        let reply =
            vault::fabric::run_command(self.command(), &self.pattern, &input, self.max_chars, self.timeout_secs)?;
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

/// Per-claim verdict from the claim judge: is this one claim backed by the
/// source? Finer-grained than the 0-3 `summary-faithfulness` axis, which never
/// looks at claim truth at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ClaimVerdict {
    /// The source states or directly implies the claim.
    Supported,
    /// The source does not address the claim (invented or over-reached).
    Unsupported,
    /// The source says the opposite, or a different number/name.
    Contradicted,
}

impl ClaimVerdict {
    /// One-char code used by the judgment cache.
    pub fn code(self) -> char {
        match self {
            Self::Supported => 'S',
            Self::Unsupported => 'U',
            Self::Contradicted => 'C',
        }
    }

    /// Inverse of [`ClaimVerdict::code`].
    pub fn from_code(c: char) -> Option<Self> {
        match c {
            'S' => Some(Self::Supported),
            'U' => Some(Self::Unsupported),
            'C' => Some(Self::Contradicted),
            _ => None,
        }
    }
}

/// Grades every claim of a note against the source, one verdict per claim.
pub trait ClaimJudge {
    /// Return exactly one verdict per entry of `claims`, in order.
    fn judge_claims(&self, kind: &str, source: &str, claims: &[String]) -> Result<Vec<ClaimVerdict>>;
}

#[cfg(test)]
/// Deterministic claim judge for tests: every claim gets `verdict`, except
/// claims containing an `overrides` key, which get that key's verdict.
#[derive(Debug)]
pub struct MockClaimJudge {
    verdict: ClaimVerdict,
    overrides: Vec<(String, ClaimVerdict)>,
    calls: AtomicUsize,
}

#[cfg(test)]
impl MockClaimJudge {
    pub fn new(verdict: ClaimVerdict) -> Self {
        Self {
            verdict,
            overrides: Vec::new(),
            calls: AtomicUsize::new(0),
        }
    }

    /// Claims whose text contains `needle` get `verdict` (builder style).
    pub fn with(mut self, needle: &str, verdict: ClaimVerdict) -> Self {
        self.overrides.push((needle.to_string(), verdict));
        self
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
impl ClaimJudge for MockClaimJudge {
    fn judge_claims(&self, _kind: &str, _source: &str, claims: &[String]) -> Result<Vec<ClaimVerdict>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(claims
            .iter()
            .map(|c| {
                self.overrides
                    .iter()
                    .find(|(needle, _)| c.contains(needle.as_str()))
                    .map(|(_, v)| *v)
                    .unwrap_or(self.verdict)
            })
            .collect())
    }
}

/// Production claim judge: runs the `judge-claims` Fabric pattern over the
/// source and a numbered claim list, and parses one verdict per claim.
#[derive(Debug, Clone)]
pub struct FabricClaimJudge {
    pub binary: String,
    pub model: String,
    /// Per-vendor credential vars (`llm.api-keys`), exported under fabric's
    /// names on the child; empty leaves fabric on its own `.env`.
    pub api_keys: ApiKeys,
    pub pattern: String,
    pub max_chars: usize,
    pub timeout_secs: u64,
}

impl FabricClaimJudge {
    pub fn new(model: impl Into<String>, api_keys: ApiKeys) -> Self {
        Self {
            binary: "fabric".to_string(),
            model: model.into(),
            api_keys,
            pattern: "judge-claims".to_string(),
            max_chars: JUDGE_INPUT_MAX_CHARS,
            timeout_secs: DEFAULT_JUDGE_TIMEOUT_SECS,
        }
    }

    /// The exact fabric child this judge runs.
    pub fn command(&self) -> Command {
        vault::fabric::command(&self.binary, &self.pattern, &self.model, &self.api_keys, 0)
    }
}

impl ClaimJudge for FabricClaimJudge {
    fn judge_claims(&self, kind: &str, source: &str, claims: &[String]) -> Result<Vec<ClaimVerdict>> {
        log::debug!(
            "FabricClaimJudge::judge_claims: kind={} pattern={} model={} source_len={} claims={}",
            kind,
            self.pattern,
            self.model,
            source.len(),
            claims.len()
        );
        if claims.is_empty() {
            return Ok(Vec::new());
        }
        let numbered: String = claims
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{}. {}\n", i + 1, c.trim()))
            .collect();
        // fabric keeps the head of the input at max_chars, so the source goes
        // last: truncation clips the source tail, never the claim list.
        let input = format!("# KIND\n{kind}\n\n# CLAIMS\n{numbered}\n# SOURCE\n{source}\n");
        let reply =
            vault::fabric::run_command(self.command(), &self.pattern, &input, self.max_chars, self.timeout_secs)?;
        parse_claim_verdicts(&reply, claims.len())
    }
}

/// Parse `N: supported|unsupported|contradicted` lines into exactly `n`
/// verdicts. Lenient on decoration (fences, bullets, bold, `N.` or `N)`), strict
/// on coverage: a missing claim number is an error, never a silent default.
pub fn parse_claim_verdicts(reply: &str, n: usize) -> Result<Vec<ClaimVerdict>> {
    let re = Regex::new(r"(?im)^[\s>*\-`]*(\d+)\s*[:.)\-]+[\s*`]*(supported|unsupported|contradicted)\b")
        .expect("static verdict regex compiles");
    let mut verdicts: Vec<Option<ClaimVerdict>> = vec![None; n];
    for cap in re.captures_iter(reply) {
        let Ok(idx) = cap[1].parse::<usize>() else {
            continue;
        };
        if idx == 0 || idx > n || verdicts[idx - 1].is_some() {
            continue;
        }
        verdicts[idx - 1] = Some(match cap[2].to_lowercase().as_str() {
            "supported" => ClaimVerdict::Supported,
            "unsupported" => ClaimVerdict::Unsupported,
            _ => ClaimVerdict::Contradicted,
        });
    }
    let missing: Vec<usize> = (1..=n).filter(|i| verdicts[i - 1].is_none()).collect();
    if !missing.is_empty() {
        bail!(
            "claim judge reply missing verdicts for claims {:?}: {:?}",
            missing,
            reply.chars().take(160).collect::<String>()
        );
    }
    Ok(verdicts.into_iter().flatten().collect())
}

#[cfg(test)]
mod tests;
