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

pub mod agreement;
pub mod backlog;
pub mod cache;
pub mod calc;
pub mod fixtures;
pub mod inject;
pub mod intents;
pub mod judge;
pub mod patterns;
pub mod regenerate;
pub mod report;
pub mod scored;
pub mod scores;
pub mod timer;
pub mod trust;
pub mod verdicts;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use eyre::{Context, Result};
use judge::JUDGE_INPUT_MAX_CHARS;

pub use fixtures::{
    CAPTURE_NOTE_FILE, Fixture, StagedTrace, judge_note_text, load, load_staged, load_staged_trace, probe_staged_trace,
    render_options_for_kind, stratified_sample,
};
pub use intents::{FabricIntentWriter, INTENT_KINDS, IntentWriter, SynthReport, synth_intents};
pub use judge::{AxisScores, ClaimJudge, ClaimVerdict, DistillationJudge, FabricClaimJudge, FabricJudge};
#[cfg(test)]
pub use judge::{MockClaimJudge, MockJudge};
pub use patterns::{PatternDirFabric, pattern_in, resolve_patterns_dir};
pub use regenerate::{RegenerateReport, regenerate_dispatcher, run_regenerate};
pub use report::{
    ArtifactMetric, CalibrationPanel, ClaimRecord, ClaimSupport, EvalReport, FlaggedClaim, GroundingMetric, KindReport,
    ListicleMetric, NoteSizeMetric, ToolsGroundingMetric, TraceError, TraceMetrics, TraceRecord,
};

/// `run_kind` of a score over the staged note as published (vs a
/// `--regenerate` candidate or a planted-error injection).
pub const RUN_KIND_BASELINE: &str = "baseline";
/// `run_kind` of a score over a planted-error mutation (`--inject`).
pub const RUN_KIND_INJECT: &str = "inject";
/// `check_name` of the three-axis distillation judge.
pub const CHECK_JUDGE_DISTILLATION: &str = "judge-distillation";
/// `check_name` of the per-claim verdict judge.
pub const CHECK_JUDGE_CLAIMS: &str = "judge-claims";

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
    /// Per-vendor credential vars (borg's `llm.api-keys`) every eval fabric
    /// call (both judges, the intent writer) exports under fabric's names on
    /// the child. Empty leaves fabric on its own `.env`.
    pub api_keys: vault::fabric::ApiKeys,
    /// Absolute dir every eval pattern (distill, judge, intent) resolves from;
    /// `None` = the deployed `vault::paths::patterns_dir()`. Set it through
    /// [`resolve_patterns_dir`].
    pub patterns_dir: Option<PathBuf>,
    /// Ignore and overwrite cached judgments.
    pub rebuild_cache: bool,
    /// When set, write a fillable calibration sheet to this path and skip metrics.
    pub emit_calibration: Option<PathBuf>,
    /// Score live distillations from this staging root instead of the golden
    /// fixtures (`fixtures::load_staged`).
    pub staged_root: Option<PathBuf>,
    /// Cap a staged run at this many traces (stratified across kinds).
    pub sample: Option<usize>,
    /// Skip both LLM judges; report only the deterministic metrics.
    pub no_judge: bool,
}

impl Default for EvalOpts {
    fn default() -> Self {
        Self {
            fixtures_dir: PathBuf::from("config/eval/distill-fixtures"),
            calibration_path: PathBuf::from("config/eval/distill-calibration.yml"),
            judge_model: String::new(),
            api_keys: vault::fabric::ApiKeys::default(),
            patterns_dir: None,
            rebuild_cache: false,
            emit_calibration: None,
            staged_root: None,
            sample: None,
            no_judge: false,
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
        "eval::run: fixtures_dir={} staged_root={:?} sample={:?} no_judge={} judge_model={} rebuild_cache={} emit={}",
        opts.fixtures_dir.display(),
        opts.staged_root,
        opts.sample,
        opts.no_judge,
        opts.judge_model,
        opts.rebuild_cache,
        opts.emit_calibration.is_some()
    );
    let fixtures = match &opts.staged_root {
        Some(root) => load_staged(root, opts.sample)?,
        None => load(&opts.fixtures_dir)?,
    };
    run_fixtures(opts, &fixtures)
}

/// [`run`] over fixtures the caller already selected (`--scored` picks its own).
pub fn run_fixtures(opts: &EvalOpts, fixtures: &[Fixture]) -> Result<EvalOutcome> {
    let calibration = load_calibration(&opts.calibration_path)?;
    let cache = cache::JudgmentCache::open(&vault::paths::borg_eval_cache_path())?;
    let (judge, claim_judge) = fabric_judges(opts)?;
    evaluate(fixtures, &calibration, &cache, &judge, &claim_judge, opts)
}

/// The production judges for `opts`: its model and credential vars, with each
/// pattern resolved from `opts.patterns_dir` when set.
pub fn fabric_judges(opts: &EvalOpts) -> Result<(FabricJudge, FabricClaimJudge)> {
    let mut judge = FabricJudge::new(opts.judge_model.clone(), opts.api_keys.clone());
    judge.pattern = pattern_in(opts.patterns_dir.as_deref(), &judge.pattern)?;
    let mut claim_judge = FabricClaimJudge::new(opts.judge_model.clone(), opts.api_keys.clone());
    claim_judge.pattern = pattern_in(opts.patterns_dir.as_deref(), &claim_judge.pattern)?;
    Ok((judge, claim_judge))
}

/// The production intent writer for `opts`, its pattern resolved from
/// `opts.patterns_dir` when set.
pub fn fabric_intent_writer(opts: &EvalOpts) -> Result<FabricIntentWriter> {
    let mut writer = FabricIntentWriter::new(opts.judge_model.clone(), opts.api_keys.clone());
    writer.pattern = pattern_in(opts.patterns_dir.as_deref(), &writer.pattern)?;
    Ok(writer)
}

/// Judge each fixture (cache-first), aggregate per kind + overall, and build the
/// calibration panel. `judge` is a trait object so tests inject a deterministic
/// [`MockJudge`]. In `--emit-calibration` mode, write the sheet and short-circuit.
/// With `opts.no_judge`, neither judge is called and only the deterministic
/// metrics are reported.
///
/// A failed judge call is captured on that trace's [`TraceRecord::errors`]
/// and the run continues: one bad reply never aborts the batch. The caller
/// decides the exit from [`EvalReport::all_judged_errored`].
pub fn evaluate(
    fixtures: &[Fixture],
    calibration: &BTreeMap<String, AxisScores>,
    cache: &cache::JudgmentCache,
    judge: &dyn DistillationJudge,
    claim_judge: &dyn ClaimJudge,
    opts: &EvalOpts,
) -> Result<EvalOutcome> {
    log::debug!(
        "eval::evaluate: fixtures={} rebuild_cache={} no_judge={}",
        fixtures.len(),
        opts.rebuild_cache,
        opts.no_judge
    );
    if opts.no_judge && opts.emit_calibration.is_some() {
        eyre::bail!("--emit-calibration needs judge scores; drop --no-judge");
    }

    // Per-kind accumulators (insertion order does not matter; sorted at report time).
    let mut per_kind: BTreeMap<String, Vec<AxisScores>> = BTreeMap::new();
    let mut all: Vec<AxisScores> = Vec::new();
    let mut new_judgments = 0usize;
    let mut new_claim_judgments = 0usize;
    let mut truncated_judgments = 0usize;
    let mut fallback_fixtures = 0usize;
    let mut calib_pairs: Vec<(u8, u8)> = Vec::new();
    let mut sheet: Vec<CalibrationRow> = Vec::new();
    // Deterministic metrics: computed once per
    // fixture alongside the judge axes, never gated by cache/judge failure.
    let mut listicle: Vec<report::ListicleMetric> = Vec::new();
    let mut note_size: Vec<report::NoteSizeMetric> = Vec::new();
    let mut grounding: Vec<GroundingMetric> = Vec::new();
    let mut artifacts: Vec<ArtifactMetric> = Vec::new();
    let mut tools_grounding: Vec<ToolsGroundingMetric> = Vec::new();
    let mut claim_support: BTreeMap<String, ClaimSupport> = BTreeMap::new();
    let mut flagged_claims: Vec<FlaggedClaim> = Vec::new();
    let mut traces: Vec<TraceRecord> = Vec::with_capacity(fixtures.len());

    for fx in fixtures {
        if fx.distilled.meta.validation.fallback_reason.is_some() {
            fallback_fixtures += 1;
        }

        // listicle-survival: applicability gate is the fixture's OWN expected
        // `declared_count` - present means "this is a listicle and must
        // survive", absent means N/A (excluded from the aggregate, not 0.0).
        let listicle_score = fx
            .distilled
            .enumeration
            .as_ref()
            .and_then(|e| e.declared_count)
            .map(|_| calc::listicle_survival(fx.distilled.enumeration.as_ref()));
        if let Some(score) = listicle_score {
            listicle.push(report::ListicleMetric {
                fixture: fx.id.clone(),
                score,
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

        let mut metrics = TraceMetrics {
            rendered_bytes,
            within_ceiling: calc::note_size_within_ceiling(rendered_bytes),
            listicle_survival: listicle_score,
            ..TraceMetrics::default()
        };
        let (grounded, quoted) = calc::quote_grounding(&fx.source, &fx.distilled);
        if quoted > 0 {
            grounding.push(GroundingMetric {
                fixture: fx.id.clone(),
                kind: fx.kind.clone(),
                grounded,
                quoted,
            });
            metrics.quote_grounding = Some(report::QuoteGrounding { grounded, quoted });
        }
        let tool_counts = calc::tools_grounding(&fx.source, &fx.distilled);
        if !tool_counts.is_empty() {
            tools_grounding.push(ToolsGroundingMetric {
                fixture: fx.id.clone(),
                kind: fx.kind.clone(),
                counts: tool_counts,
            });
            metrics.tools_grounding = Some(tool_counts);
        }
        let reasons = calc::placeholder_artifacts(&fx.distilled);
        if !reasons.is_empty() {
            metrics.placeholder_artifacts = reasons.clone();
            artifacts.push(ArtifactMetric {
                fixture: fx.id.clone(),
                reasons,
            });
        }

        // The judge sees the full note first, then as much source as the
        // input budget leaves, so a long source can never push the note out.
        let JudgedInput {
            note,
            source,
            truncated,
            content_hash,
        } = judged_input(fx);
        let mut record = TraceRecord {
            trace: fx.slug.clone(),
            fixture: fx.id.clone(),
            run_kind: RUN_KIND_BASELINE.to_string(),
            run_id: String::new(),
            rubric_version: cache::RUBRIC_VERSION.to_string(),
            judge_model: opts.judge_model.clone(),
            kind: fx.kind.clone(),
            content_hash: content_hash.clone(),
            source_truncated: truncated,
            claims: ClaimRecord::from_distilled(&fx.distilled),
            note_text: note,
            source,
            metrics,
            errors: Vec::new(),
        };

        if opts.no_judge {
            traces.push(record);
            continue;
        }

        let key = cache::CacheKey {
            fixture_id: &fx.id,
            content_hash: &content_hash,
            judge_model: &opts.judge_model,
        };
        let cached = if opts.rebuild_cache { None } else { cache.get(&key)? };
        let axis = match cached {
            Some(cj) => Some(cj),
            None => match judge.judge(&fx.kind, &record.source, &record.note_text) {
                Ok(scores) => {
                    let cj = cache::CachedJudgment {
                        scores: scores.clamped(),
                        truncated,
                    };
                    cache.put(&key, cj)?;
                    new_judgments += 1;
                    Some(cj)
                }
                Err(e) => {
                    log::warn!("eval::evaluate: {CHECK_JUDGE_DISTILLATION} failed on {}: {e:#}", fx.id);
                    record.errors.push(TraceError::new(CHECK_JUDGE_DISTILLATION, &e));
                    None
                }
            },
        };
        if let Some(cj) = axis {
            if cj.truncated {
                truncated_judgments += 1;
            }
            record.metrics.judge_distillation = Some(cj.scores);
        }

        let claims: Vec<String> = fx.distilled.claims.iter().map(|c| c.text.trim().to_string()).collect();
        let claims_hash = cache::stable_hash(&format!("{}\n{}\n{}", fx.kind, record.source, claims.join("\n")));
        let claims_key = cache::CacheKey {
            fixture_id: &fx.id,
            content_hash: &claims_hash,
            judge_model: &opts.judge_model,
        };
        let cached_verdicts = if opts.rebuild_cache { None } else { cache.get_claims(&claims_key)? };
        let verdicts = match cached_verdicts {
            Some(v) if v.len() == claims.len() => Some(v),
            _ => match judge_claims_checked(claim_judge, &fx.kind, &record.source, &claims) {
                Ok(v) => {
                    cache.put_claims(&claims_key, &v)?;
                    new_claim_judgments += 1;
                    Some(v)
                }
                Err(e) => {
                    log::warn!("eval::evaluate: {CHECK_JUDGE_CLAIMS} failed on {}: {e:#}", fx.id);
                    record.errors.push(TraceError::new(CHECK_JUDGE_CLAIMS, &e));
                    None
                }
            },
        };
        if let Some(verdicts) = &verdicts {
            let support = claim_support
                .entry(fx.kind.clone())
                .or_insert_with(|| ClaimSupport::empty(&fx.kind));
            for (claim, verdict) in claims.iter().zip(verdicts) {
                support.add(*verdict);
                if *verdict != ClaimVerdict::Supported {
                    flagged_claims.push(FlaggedClaim {
                        fixture: fx.id.clone(),
                        verdict: *verdict,
                        text: claim.clone(),
                    });
                }
            }
            record.metrics.judge_claims = Some(verdicts.clone());
        }

        if let Some(cj) = axis {
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
        traces.push(record);
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
    let claim_support: Vec<ClaimSupport> = claim_support.into_values().collect();
    let claim_support_overall = ClaimSupport::sum(OVERALL_LABEL, &claim_support);

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
        judged: !opts.no_judge,
        new_claim_judgments,
        grounding,
        tools_grounding,
        artifacts,
        claim_support,
        claim_support_overall,
        flagged_claims,
        traces,
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

/// Run the claim judge and hold it to its contract: exactly one verdict per
/// claim. A short or long reply is an error for this trace, never a guess.
fn judge_claims_checked(
    claim_judge: &dyn ClaimJudge,
    kind: &str,
    source: &str,
    claims: &[String],
) -> Result<Vec<ClaimVerdict>> {
    let v = claim_judge.judge_claims(kind, source, claims)?;
    if v.len() != claims.len() {
        eyre::bail!("claim judge returned {} verdicts for {} claims", v.len(), claims.len());
    }
    Ok(v)
}

/// Exactly what the distillation judge sees for a fixture, and its hash.
pub struct JudgedInput {
    pub note: String,
    /// The source, truncated to what the note leaves of the judge budget.
    pub source: String,
    pub truncated: bool,
    /// The distill hash: `stable_hash(kind, budgeted source, note)`. The
    /// `eval_scores` `content_hash`, so a row is current only while the note
    /// and the source budget it was judged on are unchanged.
    pub content_hash: String,
}

/// The judge input for `fx`: the full note first, then as much source as the
/// input budget leaves, so a long source can never push the note out.
pub fn judged_input(fx: &Fixture) -> JudgedInput {
    let note = judge_note_text(&fx.distilled);
    let budget = judge::source_budget(JUDGE_INPUT_MAX_CHARS, &fx.kind, &note);
    let (source, truncated) = truncate_source(&fx.source, budget);
    let content_hash = cache::stable_hash(&format!("{}\n{}\n{}", fx.kind, source, note));
    JudgedInput {
        note,
        source,
        truncated,
        content_hash,
    }
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
