//! `sb borg eval --inject`: measure the judges with planted errors instead of
//! human labels.
//!
//! Each sampled staged note is cloned and given one deterministic mutation per
//! planted class ([`PlantedClass`]). Every check scores the original and each
//! mutant, and the run reports, per check and class the check can see (the
//! applicability matrix, [`applies`]):
//! - **paired catch rate:** flags the mutant AND passes its original (a check
//!   that flags everything scores 0 here),
//! - **raw catch rate:** flags the mutant,
//! - **flag rate on the unmutated originals:** a false-alarm proxy, since
//!   production notes are not known-clean,
//! - the one-sided Wilson lower bound on the paired rate, and the applicable
//!   sample size.
//!
//! One `judge_trust` row is written per judge model, check and applicable
//! class ([`super::trust`]); the trust rule reads those rows.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::path::Path;

use chrono::{DateTime, Utc};
use eyre::{Result, bail, eyre};
use regex::Regex;
use vault::distilled::Distilled;

use super::agreement::{AgreementReport, agreement};
use super::calc::{grounding_words, wilson_lower_bound};
use super::judge::MAX_SCORE;
use super::report::{TraceMetrics, TraceRecord};
use super::scores::{CHECK_QUOTE_GROUNDING, NO_MODEL, ScoreStore, rows_from_record, rubric_version_of};
use super::trust::{TrustRow, TrustState, TrustStore, judge_check_trust};
use super::{
    CHECK_JUDGE_CLAIMS, CHECK_JUDGE_DISTILLATION, ClaimJudge, DistillationJudge, EvalOpts, EvalOutcome, Fixture,
    RUN_KIND_INJECT, cache, evaluate, load_staged, stratified_sample,
};
use crate::config::{EvalConfig, TrustConfig};

/// One deterministic mutation of a cloned `Distilled`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PlantedClass {
    /// A number in a claim becomes one the source never states.
    NumberSwap,
    /// The top claim carries a quote built from words absent from the source.
    FabricatedQuote,
    /// The first claim is removed.
    DroppedTopClaim,
    /// A claim's `who` names someone the source never mentions.
    WhoChange,
}

impl PlantedClass {
    pub const ALL: [Self; 4] = [
        Self::NumberSwap,
        Self::FabricatedQuote,
        Self::DroppedTopClaim,
        Self::WhoChange,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::NumberSwap => "number-swap",
            Self::FabricatedQuote => "fabricated-quote",
            Self::DroppedTopClaim => "dropped-top-claim",
            Self::WhoChange => "who-change",
        }
    }
}

impl fmt::Display for PlantedClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The checks `--inject` measures, in report order.
pub const INJECT_CHECKS: [&str; 3] = [CHECK_JUDGE_DISTILLATION, CHECK_JUDGE_CLAIMS, CHECK_QUOTE_GROUNDING];

/// The judge checks, each measured per judge model.
const JUDGE_CHECKS: [&str; 2] = [CHECK_JUDGE_DISTILLATION, CHECK_JUDGE_CLAIMS];

/// The applicability matrix: can `check` see a `class` mutation at all?
/// - `judge-distillation` sees `judge_note_text`, which renders every claim
///   with its quote and who: every class.
/// - `judge-claims` sees claim text only: a number swap. Dropping a claim
///   leaves the remaining claims supported, and quote and who are not in its
///   input.
/// - `quote-grounding` checks quotes against the source: a fabricated quote.
///
/// A cell that says no is reported `n/a` and never counts against the check.
pub fn applies(check: &str, class: PlantedClass) -> bool {
    match check {
        CHECK_JUDGE_DISTILLATION => true,
        CHECK_JUDGE_CLAIMS => class == PlantedClass::NumberSwap,
        CHECK_QUOTE_GROUNDING => class == PlantedClass::FabricatedQuote,
        _ => false,
    }
}

/// The classes `check` can see, in [`PlantedClass::ALL`] order.
pub fn applicable_classes(check: &str) -> Vec<PlantedClass> {
    PlantedClass::ALL.into_iter().filter(|c| applies(check, *c)).collect()
}

/// Real words, none of them likely in a tech source. A fabricated quote is
/// the first [`FABRICATED_QUOTE_WORDS`] of them the source does not contain.
const FABRICATION_WORDS: [&str; 16] = [
    "marmalade",
    "trombone",
    "porcupine",
    "saxophone",
    "tapestry",
    "walrus",
    "cinnamon",
    "hammock",
    "pelican",
    "thimble",
    "gondola",
    "artichoke",
    "flamingo",
    "harpsichord",
    "dandelion",
    "accordion",
];
const FABRICATED_QUOTE_WORDS: usize = 6;
/// Quote grounding matches on 3-word shingles; a quote any shorter would be
/// matched as a contiguous run instead.
const MIN_FABRICATED_QUOTE_WORDS: usize = 3;

/// Stand-in attributions for a who-change: the first one whose words the
/// source never uses replaces the claim's `who`.
const STAND_IN_ATTRIBUTIONS: [&str; 4] = [
    "Dr. Imogen Thistlewood",
    "Professor Bartholomew Quince",
    "@marmalade_walrus",
    "Captain Ottoline Fairweather",
];

/// Plant `class` into a clone of `distilled`, judged against `source`.
/// `None` when the class does not apply to this note (no number to swap, no
/// claim to quote or drop, no `who` to change) or no mutation the source
/// cannot back exists.
pub fn plant(class: PlantedClass, distilled: &Distilled, source: &str) -> Option<Distilled> {
    match class {
        PlantedClass::NumberSwap => swap_number(distilled, source),
        PlantedClass::FabricatedQuote => fabricate_quote(distilled, source),
        PlantedClass::DroppedTopClaim => drop_top_claim(distilled),
        PlantedClass::WhoChange => change_who(distilled, source),
    }
}

/// The first standalone number in a claim's text becomes the smallest larger
/// number the source never states, so the swap contradicts the source rather
/// than landing on another figure it happens to cite.
fn swap_number(distilled: &Distilled, source: &str) -> Option<Distilled> {
    let number = Regex::new(r"\b\d+\b").expect("static number regex compiles");
    let source_numbers: HashSet<&str> = number.find_iter(source).map(|m| m.as_str()).collect();
    for (i, claim) in distilled.claims.iter().enumerate() {
        for m in number.find_iter(&claim.text) {
            let Ok(n) = m.as_str().parse::<u64>() else {
                continue;
            };
            let Some(swapped) = (1..=1000u64)
                .filter_map(|k| n.checked_add(k))
                .map(|c| c.to_string())
                .find(|c| !source_numbers.contains(c.as_str()))
            else {
                continue;
            };
            let mut out = distilled.clone();
            out.claims[i].text = format!("{}{swapped}{}", &claim.text[..m.start()], &claim.text[m.end()..]);
            return Some(out);
        }
    }
    None
}

/// The top claim's quote becomes a run of words the source never uses.
fn fabricate_quote(distilled: &Distilled, source: &str) -> Option<Distilled> {
    if distilled.claims.is_empty() {
        return None;
    }
    let source_words: HashSet<String> = grounding_words(source).into_iter().collect();
    let words: Vec<&str> = FABRICATION_WORDS
        .iter()
        .copied()
        .filter(|w| !source_words.contains(*w))
        .take(FABRICATED_QUOTE_WORDS)
        .collect();
    if words.len() < MIN_FABRICATED_QUOTE_WORDS {
        return None;
    }
    let mut out = distilled.clone();
    out.claims[0].quote = Some(words.join(" "));
    Some(out)
}

/// The first claim is removed. Needs at least two claims, so the mutant still
/// has a claim list to judge: an empty list is a different defect.
fn drop_top_claim(distilled: &Distilled) -> Option<Distilled> {
    if distilled.claims.len() < 2 {
        return None;
    }
    let mut out = distilled.clone();
    out.claims.remove(0);
    Some(out)
}

/// The first attributed claim's `who` becomes a stand-in the source never
/// mentions.
fn change_who(distilled: &Distilled, source: &str) -> Option<Distilled> {
    let i = distilled
        .claims
        .iter()
        .position(|c| c.who.as_deref().is_some_and(|w| !w.trim().is_empty()))?;
    let current = distilled.claims[i]
        .who
        .as_deref()
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    let source_words: HashSet<String> = grounding_words(source).into_iter().collect();
    let stand_in = STAND_IN_ATTRIBUTIONS
        .iter()
        .find(|a| a.to_lowercase() != current && grounding_words(a).iter().all(|w| !source_words.contains(w)))?;
    let mut out = distilled.clone();
    out.claims[i].who = Some((*stand_in).to_string());
    Some(out)
}

/// Does `check` flag the note these metrics describe? `None` when the check
/// produced nothing (a failed judge call), so the pair is counted as an error,
/// never as a pass.
/// - `judge-distillation` flags any axis below the maximum: a perfect note
///   scores 3 on every axis, so anything less is the judge reporting a defect.
/// - `judge-claims` flags any claim that is not `supported`.
/// - `quote-grounding` flags any quote the source does not back; a note with
///   no quote passes.
pub fn flags(check: &str, metrics: &TraceMetrics) -> Option<bool> {
    match check {
        CHECK_JUDGE_DISTILLATION => metrics.judge_distillation.map(|a| {
            a.claim_coverage < MAX_SCORE || a.anchor_validity < MAX_SCORE || a.summary_faithfulness < MAX_SCORE
        }),
        CHECK_JUDGE_CLAIMS => metrics
            .judge_claims
            .as_ref()
            .map(|v| v.iter().any(|x| *x != super::ClaimVerdict::Supported)),
        CHECK_QUOTE_GROUNDING => Some(metrics.quote_grounding.is_some_and(|g| g.grounded < g.quoted)),
        _ => None,
    }
}

/// Counts for one check and class over the applicable traces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClassTally {
    /// Traces where the mutation applied and both sides were scored.
    pub n: usize,
    /// Mutant flagged AND original passed.
    pub paired_caught: usize,
    /// Mutant flagged.
    pub raw_caught: usize,
    /// Original flagged.
    pub originals_flagged: usize,
    /// Traces where the mutation applied but a side was not scored.
    pub errors: usize,
}

impl ClassTally {
    /// Add one trace: whether the check flagged its original and its mutant.
    pub fn add(&mut self, original: Option<bool>, mutant: Option<bool>) {
        let (Some(original), Some(mutant)) = (original, mutant) else {
            self.errors += 1;
            return;
        };
        self.n += 1;
        if mutant {
            self.raw_caught += 1;
        }
        if original {
            self.originals_flagged += 1;
        }
        if mutant && !original {
            self.paired_caught += 1;
        }
    }

    fn rate(count: usize, n: usize) -> f64 {
        if n == 0 { 0.0 } else { count as f64 / n as f64 }
    }

    pub fn paired_catch_rate(&self) -> f64 {
        Self::rate(self.paired_caught, self.n)
    }

    pub fn raw_catch_rate(&self) -> f64 {
        Self::rate(self.raw_caught, self.n)
    }

    pub fn flag_rate(&self) -> f64 {
        Self::rate(self.originals_flagged, self.n)
    }

    pub fn wilson_lower(&self) -> f64 {
        wilson_lower_bound(self.paired_caught, self.n)
    }
}

/// One report cell: a judge model (`""` for a deterministic check), a check
/// and a class. `tally` is `None` where the matrix says the check cannot see
/// the class (`n/a`).
#[derive(Debug, Clone, PartialEq)]
pub struct InjectCell {
    pub judge_model: String,
    pub check: &'static str,
    pub class: PlantedClass,
    pub tally: Option<ClassTally>,
}

/// One check's trust state for one judge model (`""` = deterministic).
#[derive(Debug, Clone, PartialEq)]
pub struct CheckTrust {
    pub judge_model: String,
    pub check: &'static str,
    pub state: TrustState,
}

/// Everything one `--inject` run measured.
#[derive(Debug, Clone)]
pub struct InjectReport {
    pub run_id: String,
    pub traces: usize,
    pub judge_models: Vec<String>,
    pub cells: Vec<InjectCell>,
    pub trust: Vec<CheckTrust>,
    pub policy: TrustConfig,
    /// Pairwise kappa between every rater with a current baseline row on the
    /// sampled originals: this run's judges, plus any `eval/` tool verdicts
    /// already ingested at the same distill hash.
    pub agreement: AgreementReport,
    pub score_rows_written: usize,
    pub trust_rows_written: usize,
}

impl InjectReport {
    /// True when the judges ran and no judge cell scored a single pair: every
    /// applicable judged trace errored.
    pub fn all_judged_errored(&self) -> bool {
        let judged = self
            .cells
            .iter()
            .filter(|c| !c.judge_model.is_empty())
            .filter_map(|c| c.tally);
        let (n, errors) = judged.fold((0, 0), |(n, e), t| (n + t.n, e + t.errors));
        n == 0 && errors > 0
    }

    /// The plain-text report: the "what this misses" header, one row per
    /// check, judge and class (`n/a` where the check cannot see the class),
    /// and each check's trust state.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "planted-error injection  run={}  traces={}  judges={}\n\n",
            self.run_id,
            self.traces,
            self.judge_models.join(", ")
        ));
        out.push_str(HEADER);
        out.push_str(&format!(
            "\n{:<20} {:<18} {:<18} {:>4} {:>7} {:>9} {:>6} {:>6} {:>6}\n",
            "check", "judge", "class", "n", "paired", "wilson-lb", "raw", "flag", "errors"
        ));
        for cell in &self.cells {
            let judge = judge_label(&cell.judge_model);
            match cell.tally {
                Some(t) => out.push_str(&format!(
                    "{:<20} {:<18} {:<18} {:>4} {:>7.2} {:>9.2} {:>6.2} {:>6.2} {:>6}\n",
                    cell.check,
                    judge,
                    cell.class.as_str(),
                    t.n,
                    t.paired_catch_rate(),
                    t.wilson_lower(),
                    t.raw_catch_rate(),
                    t.flag_rate(),
                    t.errors
                )),
                None => out.push_str(&format!(
                    "{:<20} {:<18} {:<18} {:>4} {:>7} {:>9} {:>6} {:>6} {:>6}\n",
                    cell.check,
                    judge,
                    cell.class.as_str(),
                    "n/a",
                    "n/a",
                    "n/a",
                    "n/a",
                    "n/a",
                    "n/a"
                )),
            }
        }
        out.push('\n');
        out.push_str(&self.agreement.render());
        out.push_str(&format!(
            "\ntrust (paired catch >= {:.2} over >= {} applicable traces, on every class the check can see, current rubric):\n",
            self.policy.min_paired_catch_rate, self.policy.min_samples
        ));
        for t in &self.trust {
            let state = match &t.state {
                TrustState::Ungated => "ungated (no model call)".to_string(),
                TrustState::Trusted => "trusted".to_string(),
                TrustState::Untrusted(failures) => format!(
                    "untrusted: {}",
                    failures
                        .iter()
                        .map(|f| format!("{} {}", f.class, f.reason))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            };
            out.push_str(&format!(
                "  {:<20} {:<18} {state}\n",
                t.check,
                judge_label(&t.judge_model)
            ));
        }
        out.push_str(&format!(
            "\n{} eval_scores rows and {} judge_trust rows written\n",
            self.score_rows_written, self.trust_rows_written
        ));
        out
    }
}

/// What every `--inject` report states before any number.
const HEADER: &str = "\
What this measures: catch rates for the four planted classes (number-swap,
fabricated-quote, dropped-top-claim, who-change), and consistency between judges.
What this misses:
  - omissions outside the planted classes
  - errors that every vendor shares
  - failures specific to long documents
  - style bias
  - the base rate of errors in production
Kappa is agreement, not correctness. The flag rate is measured on unmutated
production notes, which are not known-clean: a false-alarm proxy, not a
true-negative rate. paired = flags the mutant AND passes its original.
";

fn judge_label(model: &str) -> &str {
    if model.is_empty() { "(deterministic)" } else { model }
}

/// The `--inject` sample: every staged trace that can take at least one
/// planted class, then a deterministic stratified sample of `sample` of them.
pub fn sample_fixtures(staged_root: &Path, sample: usize) -> Result<Vec<Fixture>> {
    let all = load_staged(staged_root, None)?;
    let eligible: Vec<Fixture> = all
        .into_iter()
        .filter(|f| {
            PlantedClass::ALL
                .iter()
                .any(|c| plant(*c, &f.distilled, &f.source).is_some())
        })
        .collect();
    log::debug!(
        "eval::inject::sample_fixtures: eligible={} sample={sample}",
        eligible.len()
    );
    Ok(stratified_sample(eligible, Some(sample)))
}

/// Builds the two judges for one model's `opts`, callable from any worker.
pub type SyncJudgeFactory<'a> =
    &'a (dyn Fn(&EvalOpts) -> Result<(Box<dyn DistillationJudge>, Box<dyn ClaimJudge>)> + Sync);

/// The settings one `--inject` run needs beyond the fixtures.
pub struct InjectRun<'a> {
    /// Shared judge settings (keys, patterns dir, `rebuild_cache`).
    pub opts: &'a EvalOpts,
    /// `judge-models`, `trust`, `inject-workers`.
    pub eval: &'a EvalConfig,
    /// The judgment cache file; the workers share one connection to it.
    pub cache_path: &'a Path,
    pub now: DateTime<Utc>,
}

/// Each judged group's index and its records, as one worker returns them.
type WorkerRecords = Vec<(usize, Vec<TraceRecord>)>;

/// One trace's original and mutants, original first.
struct TraceGroup {
    fixtures: Vec<Fixture>,
    /// `None` for the original, aligned with `fixtures`.
    classes: Vec<Option<PlantedClass>>,
}

/// Plant every class into every fixture, judge originals and mutants with
/// every model in `eval.judge-models`, write the `eval_scores` rows
/// (originals as `baseline`, mutants as `inject` with run id
/// `<run>:<class>`) and the `judge_trust` rows, and return the report.
pub fn run_inject(
    fixtures: &[Fixture],
    run: &InjectRun,
    scores: &ScoreStore,
    trust: &TrustStore,
    judges: SyncJudgeFactory,
) -> Result<InjectReport> {
    let judge_models = &run.eval.judge_models;
    log::debug!(
        "eval::inject::run_inject: fixtures={} judge_models={judge_models:?} workers={}",
        fixtures.len(),
        run.eval.inject_workers
    );
    if judge_models.is_empty() {
        bail!(
            "eval.judge-models is empty: name the judge models --inject measures in borg.yml \
             (one judge_trust row is written per model, check and class)"
        );
    }
    if run.eval.inject_workers == 0 {
        bail!("eval.inject-workers must be at least 1");
    }
    let run_id = format!("inject-{}", run.now.format("%Y%m%dT%H%M%SZ"));
    let stamp = run.now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    let groups: Vec<TraceGroup> = fixtures
        .iter()
        .map(|fx| {
            let mut group = TraceGroup {
                fixtures: vec![fx.clone()],
                classes: vec![None],
            };
            for class in PlantedClass::ALL {
                if let Some(distilled) = plant(class, &fx.distilled, &fx.source) {
                    group.fixtures.push(Fixture {
                        distilled,
                        ..fx.clone()
                    });
                    group.classes.push(Some(class));
                }
            }
            group
        })
        .filter(|g| g.fixtures.len() > 1)
        .collect();

    let mut records_by_model: Vec<Vec<Vec<TraceRecord>>> = Vec::with_capacity(judge_models.len());
    for model in judge_models {
        let model_opts = EvalOpts {
            judge_model: model.clone(),
            no_judge: false,
            emit_calibration: None,
            ..run.opts.clone()
        };
        records_by_model.push(judge_groups(&groups, &model_opts, run, judges)?);
    }

    let mut score_rows_written = 0usize;
    for records in &records_by_model {
        for (group, group_records) in groups.iter().zip(records) {
            for (class, record) in group.classes.iter().zip(group_records) {
                let mut record = record.clone();
                if let Some(class) = class {
                    record.run_kind = RUN_KIND_INJECT.to_string();
                    record.run_id = format!("{run_id}:{class}");
                }
                for row in rows_from_record(&record, true, &stamp) {
                    scores.put(&row)?;
                    score_rows_written += 1;
                }
            }
        }
    }

    let mut cells = Vec::new();
    for (model, records) in judge_models.iter().zip(&records_by_model) {
        for check in JUDGE_CHECKS {
            for class in PlantedClass::ALL {
                cells.push(InjectCell {
                    judge_model: model.clone(),
                    check,
                    class,
                    tally: applies(check, class).then(|| tally(&groups, records, check, class)),
                });
            }
        }
    }
    // Deterministic metrics do not depend on the judge: read the first model's.
    for class in PlantedClass::ALL {
        cells.push(InjectCell {
            judge_model: NO_MODEL.to_string(),
            check: CHECK_QUOTE_GROUNDING,
            class,
            tally: applies(CHECK_QUOTE_GROUNDING, class)
                .then(|| tally(&groups, &records_by_model[0], CHECK_QUOTE_GROUNDING, class)),
        });
    }

    let mut trust_rows_written = 0usize;
    for cell in &cells {
        let Some(t) = cell.tally else {
            continue;
        };
        let rubric = rubric_version_of(cell.check).ok_or_else(|| eyre!("no rubric version for {}", cell.check))?;
        trust.put(&TrustRow {
            judge_model: cell.judge_model.clone(),
            check_name: cell.check.to_string(),
            planted_class: cell.class.as_str().to_string(),
            rubric_version: rubric.to_string(),
            paired_catch_rate: t.paired_catch_rate(),
            raw_catch_rate: t.raw_catch_rate(),
            flag_rate: t.flag_rate(),
            wilson_lower: t.wilson_lower(),
            sample_size: t.n,
            measured_at: stamp.clone(),
        })?;
        trust_rows_written += 1;
    }

    // Read back after the writes, so the agreement covers this run's judges
    // and any tool verdicts already ingested for the same notes.
    let mut current = Vec::new();
    for group_records in &records_by_model[0] {
        let original = &group_records[0];
        current.extend(scores.baseline_rows_at(&original.trace, &original.content_hash)?);
    }
    let agreement = agreement(&current);

    let rows = trust.all()?;
    let mut trust_states = Vec::new();
    for model in judge_models {
        for check in JUDGE_CHECKS {
            trust_states.push(CheckTrust {
                judge_model: model.clone(),
                check,
                state: judge_check_trust(model, check, &rows, &run.eval.trust),
            });
        }
    }
    trust_states.push(CheckTrust {
        judge_model: NO_MODEL.to_string(),
        check: CHECK_QUOTE_GROUNDING,
        state: judge_check_trust(NO_MODEL, CHECK_QUOTE_GROUNDING, &rows, &run.eval.trust),
    });

    Ok(InjectReport {
        run_id,
        traces: groups.len(),
        judge_models: judge_models.clone(),
        cells,
        trust: trust_states,
        policy: run.eval.trust.clone(),
        agreement,
        score_rows_written,
        trust_rows_written,
    })
}

/// Tally `check` on `class` over every group that carries that mutant.
fn tally(groups: &[TraceGroup], records: &[Vec<TraceRecord>], check: &str, class: PlantedClass) -> ClassTally {
    let mut t = ClassTally::default();
    for (group, group_records) in groups.iter().zip(records) {
        let Some(at) = group.classes.iter().position(|c| *c == Some(class)) else {
            continue;
        };
        t.add(
            flags(check, &group_records[0].metrics),
            flags(check, &group_records[at].metrics),
        );
    }
    t
}

/// Judge every group with one model on `eval.inject-workers` threads. The
/// workers share one judgment cache connection (see
/// [`cache::JudgmentCache`]) and each builds its own judges; a group
/// stays on one worker, original first, so a mutant whose claim list is
/// unchanged reuses the original's cached claim verdicts. Returns one record
/// list per group, aligned with the group's fixtures.
fn judge_groups(
    groups: &[TraceGroup],
    model_opts: &EvalOpts,
    run: &InjectRun,
    judges: SyncJudgeFactory,
) -> Result<Vec<Vec<TraceRecord>>> {
    let workers = run.eval.inject_workers.min(groups.len()).max(1);
    let cache = cache::JudgmentCache::open(run.cache_path)?;
    let cache = &cache;
    let results: Vec<Result<WorkerRecords>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..workers)
            .map(|w| {
                s.spawn(move || -> Result<WorkerRecords> {
                    let (judge, claim_judge) = judges(model_opts)?;
                    let mut out = Vec::new();
                    for (i, group) in groups.iter().enumerate().filter(|(i, _)| i % workers == w) {
                        let outcome = evaluate(
                            &group.fixtures,
                            &BTreeMap::new(),
                            cache,
                            &*judge,
                            &*claim_judge,
                            model_opts,
                        )?;
                        let EvalOutcome::Report(report) = outcome else {
                            bail!("evaluate returned a calibration sheet for an --inject run");
                        };
                        log::debug!(
                            "eval::inject::judge_groups: model={} trace={} judged",
                            model_opts.judge_model,
                            group.fixtures[0].slug
                        );
                        out.push((i, report.traces));
                    }
                    Ok(out)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| Err(eyre!("an --inject worker panicked"))))
            .collect()
    });
    let mut by_group: Vec<Option<Vec<TraceRecord>>> = (0..groups.len()).map(|_| None).collect();
    for result in results {
        for (i, records) in result? {
            by_group[i] = Some(records);
        }
    }
    by_group
        .into_iter()
        .enumerate()
        .map(|(i, r)| r.ok_or_else(|| eyre!("no records for inject group {i}")))
        .collect()
}

#[cfg(test)]
mod tests;
