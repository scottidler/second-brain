//! `sb borg eval` — distillation-quality eval harness.
//!
//! Measures how faithfully the current distillers represent their sources,
//! using golden `(source, distilled)` fixtures scored by a blind LLM judge on a
//! three-axis 0-3 rubric (claim coverage / anchor validity / summary
//! faithfulness). Structurally parallel to `sb oracle eval` (`oracle/src/eval`):
//! a load/evaluate split, an FNV-keyed SQLite judgment cache, and a calibration
//! panel with a trust gate.
//!
//! Library-only: this module returns typed data; `sb` renders it. The judge is
//! injected via the [`judge::DistillationJudge`] trait, and fixture loading
//! ([`load`]) is split from scoring ([`evaluate`]) so the scoring pipeline is
//! unit-testable against a [`judge::MockJudge`] without a live fabric call.

pub mod cache;
pub mod calc;
pub mod fixtures;
pub mod judge;
pub mod report;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use eyre::{Context, Result};

pub use fixtures::{Fixture, judge_note_text, load, render_options_for_kind};
pub use judge::{AxisScores, DistillationJudge, FabricJudge, MockJudge};
pub use report::{CalibrationPanel, EvalReport, KindReport, ListicleMetric, NoteSizeMetric};

/// Char budget for the source text shown to the judge. Larger than the oracle
/// eval's note budget (8K) because claim coverage requires the judge to see the
/// whole source; long-transcript fixtures still exceed it and are flagged.
const JUDGE_SOURCE_MAX_CHARS: usize = 24_000;
/// Label for the aggregate ("all kinds") row in the report.
pub const OVERALL_LABEL: &str = "ALL";

/// CLI-derived options for a distillation eval run.
#[derive(Debug, Clone)]
pub struct EvalOpts {
    /// Root of the fixture tree (`<root>/<kind>/<slug>/{source.md,distilled.yml}`).
    pub fixtures_dir: PathBuf,
    /// Hand-labeled calibration file (`fixture -> human axis scores`). Optional;
    /// absent file = uncalibrated run.
    pub calibration_path: PathBuf,
    /// Judge model name; empty = fabric's default model.
    pub judge_model: String,
    /// Ignore and overwrite cached judgments.
    pub rebuild_cache: bool,
    /// When set, write a fillable calibration sheet to this path and skip metrics.
    pub emit_calibration: Option<PathBuf>,
}

impl Default for EvalOpts {
    fn default() -> Self {
        Self {
            fixtures_dir: PathBuf::from("config/eval/distill-fixtures"),
            calibration_path: PathBuf::from("config/eval/distill-calibration.yml"),
            judge_model: String::new(),
            rebuild_cache: false,
            emit_calibration: None,
        }
    }
}

/// Outcome of [`run`]: either the metrics report, or the path of the
/// calibration sheet that was written (`--emit-calibration` mode).
pub enum EvalOutcome {
    Report(Box<EvalReport>),
    CalibrationSheet(PathBuf),
}

/// One row of the `--emit-calibration` sheet the operator fills with `human-*`
/// scores, then copies into the calibration file.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
struct CalibrationRow {
    fixture: String,
    kind: String,
    claim_coverage: u8,
    anchor_validity: u8,
    summary_faithfulness: u8,
    human_claim_coverage: Option<u8>,
    human_anchor_validity: Option<u8>,
    human_summary_faithfulness: Option<u8>,
}

/// Hand-labeled calibration entry (`config/eval/distill-calibration.yml`).
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
struct CalibrationLabel {
    fixture: String,
    claim_coverage: u8,
    anchor_validity: u8,
    summary_faithfulness: u8,
}

/// Load the optional hand-labeled calibration set: `fixture id -> human axis
/// scores`. A missing file is a clean "uncalibrated" run (not an error); a
/// present-but-malformed file IS an error (fail loud).
fn load_calibration(path: &Path) -> Result<BTreeMap<String, AxisScores>> {
    log::debug!("eval::load_calibration: path={}", path.display());
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let text = std::fs::read_to_string(path).with_context(|| format!("reading calibration file {}", path.display()))?;
    let labels: Vec<CalibrationLabel> =
        serde_yaml::from_str(&text).with_context(|| format!("parsing calibration file {}", path.display()))?;
    let mut map = BTreeMap::new();
    for l in labels {
        map.insert(
            l.fixture,
            AxisScores {
                claim_coverage: l.claim_coverage,
                anchor_validity: l.anchor_validity,
                summary_faithfulness: l.summary_faithfulness,
            }
            .clamped(),
        );
    }
    log::debug!("eval::load_calibration: loaded {} labels", map.len());
    Ok(map)
}

/// Run the eval end to end: load fixtures + calibration, open the cache, and
/// evaluate with the production [`FabricJudge`].
pub fn run(opts: &EvalOpts) -> Result<EvalOutcome> {
    log::debug!(
        "eval::run: fixtures_dir={} judge_model={} rebuild_cache={} emit={}",
        opts.fixtures_dir.display(),
        opts.judge_model,
        opts.rebuild_cache,
        opts.emit_calibration.is_some()
    );
    let fixtures = load(&opts.fixtures_dir)?;
    let calibration = load_calibration(&opts.calibration_path)?;
    let cache = cache::JudgmentCache::open(&vault::paths::borg_eval_cache_path())?;
    let judge = FabricJudge::new(opts.judge_model.clone());
    evaluate(&fixtures, &calibration, &cache, &judge, opts)
}

/// Judge each fixture (cache-first), aggregate per kind + overall, and build the
/// calibration panel. `judge` is a trait object so tests inject a deterministic
/// [`MockJudge`]. In `--emit-calibration` mode, write the sheet and short-circuit.
pub fn evaluate(
    fixtures: &[Fixture],
    calibration: &BTreeMap<String, AxisScores>,
    cache: &cache::JudgmentCache,
    judge: &dyn DistillationJudge,
    opts: &EvalOpts,
) -> Result<EvalOutcome> {
    log::debug!(
        "eval::evaluate: fixtures={} rebuild_cache={}",
        fixtures.len(),
        opts.rebuild_cache
    );

    // Per-kind accumulators (insertion order does not matter; sorted at report time).
    let mut per_kind: BTreeMap<String, Vec<AxisScores>> = BTreeMap::new();
    let mut all: Vec<AxisScores> = Vec::new();
    let mut new_judgments = 0usize;
    let mut truncated_judgments = 0usize;
    let mut fallback_fixtures = 0usize;
    let mut calib_pairs: Vec<(u8, u8)> = Vec::new();
    let mut sheet: Vec<CalibrationRow> = Vec::new();
    // Deterministic metrics (zero judge calls, Phase 7b): computed once per
    // fixture alongside the judge axes, never gated by cache/judge failure.
    let mut listicle: Vec<report::ListicleMetric> = Vec::new();
    let mut note_size: Vec<report::NoteSizeMetric> = Vec::new();

    for fx in fixtures {
        if fx.distilled.meta.validation.fallback_reason.is_some() {
            fallback_fixtures += 1;
        }

        // listicle-survival: applicability gate is the fixture's OWN expected
        // `declared_count` - present means "this is a listicle and must
        // survive", absent means N/A (excluded from the aggregate, not 0.0).
        if fx
            .distilled
            .enumeration
            .as_ref()
            .and_then(|e| e.declared_count)
            .is_some()
        {
            listicle.push(report::ListicleMetric {
                fixture: fx.id.clone(),
                score: calc::listicle_survival(fx.distilled.enumeration.as_ref()),
            });
        }

        // note-size: enforced across every fixture, rendered with the same
        // per-kind transcript policy a real publish/backfill call site uses.
        let render_options = fixtures::render_options_for_kind(&fx.kind, &fx.distilled);
        let rendered_bytes = distillers::render(&fx.distilled, render_options).body_markdown.len();
        note_size.push(report::NoteSizeMetric {
            fixture: fx.id.clone(),
            rendered_bytes,
            within_ceiling: calc::note_size_within_ceiling(rendered_bytes),
        });

        let note = judge_note_text(&fx.distilled);
        let (source, truncated) = truncate_source(&fx.source, JUDGE_SOURCE_MAX_CHARS);
        // The content hash covers exactly what the judge sees.
        let content_hash = cache::stable_hash(&format!("{}\n{}\n{}", fx.kind, source, note));

        let key = cache::CacheKey {
            fixture_id: &fx.id,
            content_hash: &content_hash,
            judge_model: &opts.judge_model,
        };
        let cached = if opts.rebuild_cache { None } else { cache.get(&key)? };
        let cj = match cached {
            Some(cj) => cj,
            None => {
                let scores = judge
                    .judge(&fx.kind, &source, &note)
                    .with_context(|| format!("judging fixture {}", fx.id))?
                    .clamped();
                let cj = cache::CachedJudgment { scores, truncated };
                cache.put(&key, cj)?;
                new_judgments += 1;
                cj
            }
        };
        if cj.truncated {
            truncated_judgments += 1;
        }

        per_kind.entry(fx.kind.clone()).or_default().push(cj.scores);
        all.push(cj.scores);

        if opts.emit_calibration.is_some() {
            sheet.push(CalibrationRow {
                fixture: fx.id.clone(),
                kind: fx.kind.clone(),
                claim_coverage: cj.scores.claim_coverage,
                anchor_validity: cj.scores.anchor_validity,
                summary_faithfulness: cj.scores.summary_faithfulness,
                human_claim_coverage: None,
                human_anchor_validity: None,
                human_summary_faithfulness: None,
            });
        }
        if let Some(human) = calibration.get(&fx.id) {
            // One (human, judge) pair per axis so the 0-3 calibration math reuses
            // the oracle eval's boundary-precision/recall + kappa directly.
            calib_pairs.push((human.claim_coverage, cj.scores.claim_coverage));
            calib_pairs.push((human.anchor_validity, cj.scores.anchor_validity));
            calib_pairs.push((human.summary_faithfulness, cj.scores.summary_faithfulness));
        }
    }

    if let Some(path) = &opts.emit_calibration {
        let yaml = serde_yaml::to_string(&sheet).context("serializing calibration sheet")?;
        std::fs::write(path, yaml).with_context(|| format!("writing calibration sheet {}", path.display()))?;
        log::debug!(
            "eval::evaluate: wrote calibration sheet rows={} path={}",
            sheet.len(),
            path.display()
        );
        return Ok(EvalOutcome::CalibrationSheet(path.clone()));
    }

    let kinds: Vec<KindReport> = per_kind
        .iter()
        .map(|(kind, scores)| KindReport::aggregate(kind, scores))
        .collect();
    let overall = KindReport::aggregate(OVERALL_LABEL, &all);
    let calibration_panel = report::calibration_panel(&calib_pairs);
    let listicle_scores: Vec<f64> = listicle.iter().map(|m| m.score).collect();
    let listicle_aggregate = calc::listicle_aggregate(&listicle_scores);

    let report = EvalReport {
        judge_model: opts.judge_model.clone(),
        total_fixtures: fixtures.len(),
        total_judgments: all.len(),
        new_judgments,
        truncated_judgments,
        fallback_fixtures,
        kinds,
        overall,
        calibration: calibration_panel,
        listicle,
        listicle_aggregate,
        note_size,
    };
    log::debug!(
        "eval::evaluate: done fixtures={} new_judgments={} composite={:.4} listicle_aggregate={:?}",
        report.total_fixtures,
        report.new_judgments,
        report.overall.composite,
        report.listicle_aggregate,
    );
    Ok(EvalOutcome::Report(Box::new(report)))
}

/// Truncate the source to `max_chars` on a char boundary (never a byte slice).
/// Returns `(text, truncated)`.
fn truncate_source(source: &str, max_chars: usize) -> (String, bool) {
    if source.chars().count() <= max_chars {
        return (source.to_string(), false);
    }
    (source.chars().take(max_chars).collect(), true)
}

#[cfg(test)]
mod tests;
