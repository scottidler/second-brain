//! `sb borg eval --inject`: measure the judges with planted errors instead of
//! human labels.
//!
//! Each sampled staged note is cloned and given one deterministic mutation per
//! planted class ([`PlantedClass`]). Every check scores the original and each
//! mutant, and the run reports, per check and class the check can see (the
//! applicability matrix, [`applies`]):
//! - **paired catch rate:** flags the planted unit on the mutant AND passes the
//!   same unit on its original (a check that flags everything scores 0 here),
//! - **raw catch rate:** flags the planted unit on the mutant,
//! - **flag rate on the unmutated originals:** the planted unit flagged on the
//!   original, a false-alarm proxy, since production notes are not known-clean,
//! - the one-sided Wilson lower bound on the paired rate, and the applicable
//!   sample size.
//!
//! The unit is the planted claim for a per-claim check (`judge-claims`
//! returns one verdict per claim, so the metric reads the verdict at the
//! mutated index) and the whole note for a per-note check
//! (`judge-distillation` scores axes, `quote-grounding` counts quotes). A
//! note-level pair for `judge-claims` measured the distiller as much as the
//! judge: on 2026-10-18 it read 0.59 over-window while the judge caught the
//! planted swap on 25 of 27, because the originals carried other claims the
//! judge flagged (`docs/design/2026-10-17-claim-judge-full-source.md`,
//! the 2026-10-18 ripr results section).
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
use super::fixtures::stratified_order;
use super::judge::{JUDGE_INPUT_MAX_CHARS, MAX_SCORE};
use super::report::{ClaimsOutcome, TraceMetrics, TraceRecord};
use super::scores::{CHECK_QUOTE_GROUNDING, NO_MODEL, ScoreStore, rows_from_record, rubric_version_of};
use super::trust::{TrustRow, TrustState, TrustStore, judge_check_trust};
use super::{
    CHECK_JUDGE_CLAIMS, CHECK_JUDGE_DISTILLATION, ClaimJudge, DistillationJudge, EvalOpts, EvalOutcome, Fixture,
    RUN_KIND_INJECT, cache, evaluate, load_staged,
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

/// One planted mutation: the mutated note and the claim it touched.
#[derive(Debug, Clone)]
pub struct Planted {
    pub distilled: Distilled,
    /// Index into `distilled.claims` of the one claim the mutation changed.
    /// `None` for a dropped claim: the list changed, no claim did, so no
    /// verdict on the mutant lines up with one on the original.
    pub claim: Option<usize>,
}

/// Plant `class` into a clone of `distilled`, judged against `source`.
/// `None` when the class does not apply to this note (no number to swap, no
/// claim to quote or drop, no `who` to change) or no mutation the source
/// cannot back exists.
pub fn plant(class: PlantedClass, distilled: &Distilled, source: &str) -> Option<Planted> {
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
fn swap_number(distilled: &Distilled, source: &str) -> Option<Planted> {
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
            return Some(Planted {
                distilled: out,
                claim: Some(i),
            });
        }
    }
    None
}

/// The top claim's quote becomes a run of words the source never uses.
fn fabricate_quote(distilled: &Distilled, source: &str) -> Option<Planted> {
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
    Some(Planted {
        distilled: out,
        claim: Some(0),
    })
}

/// The first claim is removed. Needs at least two claims, so the mutant still
/// has a claim list to judge: an empty list is a different defect.
fn drop_top_claim(distilled: &Distilled) -> Option<Planted> {
    if distilled.claims.len() < 2 {
        return None;
    }
    let mut out = distilled.clone();
    out.claims.remove(0);
    Some(Planted {
        distilled: out,
        claim: None,
    })
}

/// The first attributed claim's `who` becomes a stand-in the source never
/// mentions.
fn change_who(distilled: &Distilled, source: &str) -> Option<Planted> {
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
    Some(Planted {
        distilled: out,
        claim: Some(i),
    })
}

/// Does `check` flag the planted unit these metrics describe? `None` when the
/// check produced nothing (a failed judge call, or no verdict at `claim`), so
/// the pair is counted as an error, never as a pass. A not-judgeable
/// `judge-claims` side is also `None` here; [`tally`] excludes that pair
/// first ([`not_judgeable`]), so it never reaches the error count.
/// - `judge-distillation` flags any axis below the maximum: a perfect note
///   scores 3 on every axis, so anything less is the judge reporting a defect.
/// - `judge-claims` with `claim = Some(i)` flags a verdict at `i` that is not
///   `supported`; with `None` it flags any claim that is not `supported` (the
///   production meaning, which `--remediate` acts on).
/// - `quote-grounding` flags any quote the source does not back; a note with
///   no quote passes.
pub fn flags(check: &str, metrics: &TraceMetrics, claim: Option<usize>) -> Option<bool> {
    match check {
        CHECK_JUDGE_DISTILLATION => metrics.judge_distillation.map(|a| {
            a.claim_coverage < MAX_SCORE || a.anchor_validity < MAX_SCORE || a.summary_faithfulness < MAX_SCORE
        }),
        CHECK_JUDGE_CLAIMS => {
            let verdicts = metrics.judge_claims.verdicts()?;
            match claim {
                Some(i) => verdicts.get(i).map(|x| *x != super::ClaimVerdict::Supported),
                None => Some(verdicts.iter().any(|x| *x != super::ClaimVerdict::Supported)),
            }
        }
        CHECK_QUOTE_GROUNDING => Some(metrics.quote_grounding.is_some_and(|g| g.grounded < g.quoted)),
        _ => None,
    }
}

/// Is `check` not judgeable on these metrics? Only `judge-claims` can be: its
/// source was over `eval.claims-source-max-chars`, so it made no call. Such a
/// pair is out of trust math, counted in neither `n` nor `errors`.
pub fn not_judgeable(check: &str, metrics: &TraceMetrics) -> bool {
    check == CHECK_JUDGE_CLAIMS && matches!(metrics.judge_claims, ClaimsOutcome::NotJudgeable { .. })
}

/// Counts for one check and class over the applicable traces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClassTally {
    /// Traces where the mutation applied and both sides were scored.
    pub n: usize,
    /// The planted unit flagged on the mutant AND passed on the original.
    pub paired_caught: usize,
    /// The planted unit flagged on the mutant.
    pub raw_caught: usize,
    /// The planted unit flagged on the original.
    pub originals_flagged: usize,
    /// Traces where the mutation applied but a side was not scored.
    pub errors: usize,
    /// Traces where the mutation applied but the check could not judge the
    /// source (over the claims cap): in neither `n` nor `errors`.
    pub not_judgeable: usize,
}

impl ClassTally {
    /// Add one trace: whether the check flagged the planted unit on its
    /// original and on its mutant.
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

    /// Add one trace the check could not judge: excluded from the rates.
    pub fn exclude(&mut self) {
        self.not_judgeable += 1;
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

    /// Every pair the mutation applied to: scored, errored and excluded.
    pub fn pairs(&self) -> usize {
        self.n + self.errors + self.not_judgeable
    }
}

/// One cell's pairs split by the original's source length, so a long-source
/// result is visible on its own line instead of averaged into the whole.
/// - `fits`: at most [`JUDGE_INPUT_MAX_CHARS`], the old 24k judge budget.
/// - `over_window`: over that budget, at most `eval.claims-source-max-chars`.
/// - `over_cap`: over the cap; `judge-claims` makes no call there, so its
///   pairs land in `not_judgeable`.
///
/// A report line only: `judge_trust` rows carry the whole-sample tally.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SourceBuckets {
    pub fits: ClassTally,
    pub over_window: ClassTally,
    pub over_cap: ClassTally,
}

impl SourceBuckets {
    /// The cap is checked first: a legal cap below the 24k budget still puts
    /// an over-cap pair in `over_cap`, never in `fits`.
    fn of_mut(&mut self, source_chars: usize, cap: usize) -> &mut ClassTally {
        if source_chars > cap {
            &mut self.over_cap
        } else if source_chars <= JUDGE_INPUT_MAX_CHARS {
            &mut self.fits
        } else {
            &mut self.over_window
        }
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
    /// The same pairs by source length; `Some` exactly when `tally` is.
    pub buckets: Option<SourceBuckets>,
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
    /// `eval.claims-source-max-chars` this run used: the over-cap boundary.
    pub claims_source_max_chars: usize,
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
        out.push_str(&format!(
            "\npairs by source length: fits (<= {JUDGE_INPUT_MAX_CHARS} chars) | over-window (<= {} chars) | over-cap\n",
            self.claims_source_max_chars
        ));
        out.push_str("paired catch is over the scored pairs; judge-claims makes no call over-cap\n");
        for cell in &self.cells {
            let Some(b) = cell.buckets else {
                continue;
            };
            out.push_str(&format!(
                "  {:<20} {:<18} {:<18} pairs fits | over-window | over-cap: {} | {} | {}   paired catch fits | over-window: {} | {}\n",
                cell.check,
                judge_label(&cell.judge_model),
                cell.class.as_str(),
                b.fits.pairs(),
                b.over_window.pairs(),
                b.over_cap.pairs(),
                bucket_rate(&b.fits),
                bucket_rate(&b.over_window),
            ));
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
true-negative rate. paired = flags the planted unit on the mutant AND passes
it on the original. The unit is the mutated claim for judge-claims (one
verdict per claim) and the whole note for judge-distillation and
quote-grounding.
";

fn bucket_rate(t: &ClassTally) -> String {
    if t.n == 0 {
        "n/a".to_string()
    } else {
        format!("{:.2} (n={})", t.paired_catch_rate(), t.n)
    }
}

fn judge_label(model: &str) -> &str {
    if model.is_empty() { "(deterministic)" } else { model }
}

/// One (check, class) pair the sample could not fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShortPair {
    pub check: &'static str,
    pub class: PlantedClass,
    /// Plantable, judgeable traces the sample holds for this pair.
    pub have: usize,
    /// `--sample`: the floor every pair was grown toward.
    pub want: usize,
}

impl fmt::Display for ShortPair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} has {} of {}", self.check, self.class, self.have, self.want)
    }
}

/// The `--inject` sample and the pairs it could not fill.
#[derive(Debug, Clone)]
pub struct InjectSample {
    pub fixtures: Vec<Fixture>,
    /// Empty when every applicable pair reached `--sample`; otherwise the
    /// eligible pool ran out first and these are the classes that fell short.
    pub short: Vec<ShortPair>,
}

/// Can `check` read this source at all? Only `judge-claims` has a cap.
fn judgeable(check: &str, source_chars: usize, claims_cap: usize) -> bool {
    check != CHECK_JUDGE_CLAIMS || source_chars <= claims_cap
}

/// The `--inject` sample. Start from every staged trace that can take at
/// least one planted class, in the deterministic kind-stratified order. Take
/// the first `sample`, then keep adding the next trace that helps a short
/// pair until every (check, class) pair that [`applies`] holds `sample`
/// plantable, judgeable traces (judgeable: source chars at most
/// `claims_cap`, for `judge-claims`), or the pool runs out. The result is
/// sorted by id and never smaller than the plain stratified sample.
pub fn sample_fixtures(staged_root: &Path, sample: usize, claims_cap: usize) -> Result<InjectSample> {
    let all = load_staged(staged_root, None)?;
    let eligible: Vec<Fixture> = all
        .into_iter()
        .filter(|f| {
            PlantedClass::ALL
                .iter()
                .any(|c| plant(*c, &f.distilled, &f.source).is_some())
        })
        .collect();
    let pool = eligible.len();
    let order = stratified_order(eligible);

    let pairs: Vec<(&'static str, PlantedClass)> = INJECT_CHECKS
        .into_iter()
        .flat_map(|check| applicable_classes(check).into_iter().map(move |class| (check, class)))
        .collect();
    let contributes = |f: &Fixture| -> Vec<bool> {
        let chars = f.source.chars().count();
        pairs
            .iter()
            .map(|(check, class)| {
                judgeable(check, chars, claims_cap) && plant(*class, &f.distilled, &f.source).is_some()
            })
            .collect()
    };
    let mut have = vec![0usize; pairs.len()];
    let mut picked: Vec<Fixture> = Vec::new();
    for (i, f) in order.into_iter().enumerate() {
        let c = contributes(&f);
        let helps_short_pair = c.iter().zip(&have).any(|(yes, h)| *yes && *h < sample);
        if i >= sample && !helps_short_pair {
            continue;
        }
        for (h, yes) in have.iter_mut().zip(&c) {
            *h += usize::from(*yes);
        }
        picked.push(f);
        if i + 1 >= sample && have.iter().all(|h| *h >= sample) {
            break;
        }
    }
    let short: Vec<ShortPair> = pairs
        .iter()
        .zip(&have)
        .filter(|(_, h)| **h < sample)
        .map(|((check, class), h)| ShortPair {
            check,
            class: *class,
            have: *h,
            want: sample,
        })
        .collect();
    picked.sort_by(|a, b| a.id.cmp(&b.id));
    log::debug!(
        "eval::inject::sample_fixtures: eligible={pool} sample={sample} picked={} short={}",
        picked.len(),
        short.len()
    );
    Ok(InjectSample {
        fixtures: picked,
        short,
    })
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
    /// The mutated claim's index ([`Planted::claim`]), aligned with
    /// `fixtures`; `None` for the original and for a dropped claim.
    claims: Vec<Option<usize>>,
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
                claims: vec![None],
            };
            for class in PlantedClass::ALL {
                if let Some(planted) = plant(class, &fx.distilled, &fx.source) {
                    group.fixtures.push(Fixture {
                        distilled: planted.distilled,
                        ..fx.clone()
                    });
                    group.classes.push(Some(class));
                    group.claims.push(planted.claim);
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

    let cap = run.opts.claims_source_max_chars;
    let mut cells = Vec::new();
    for (model, records) in judge_models.iter().zip(&records_by_model) {
        for check in JUDGE_CHECKS {
            for class in PlantedClass::ALL {
                let measured = applies(check, class).then(|| tally(&groups, records, check, class, cap));
                cells.push(InjectCell {
                    judge_model: model.clone(),
                    check,
                    class,
                    tally: measured.map(|m| m.0),
                    buckets: measured.map(|m| m.1),
                });
            }
        }
    }
    // Deterministic metrics do not depend on the judge: read the first model's.
    for class in PlantedClass::ALL {
        let measured = applies(CHECK_QUOTE_GROUNDING, class)
            .then(|| tally(&groups, &records_by_model[0], CHECK_QUOTE_GROUNDING, class, cap));
        cells.push(InjectCell {
            judge_model: NO_MODEL.to_string(),
            check: CHECK_QUOTE_GROUNDING,
            class,
            tally: measured.map(|m| m.0),
            buckets: measured.map(|m| m.1),
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
        claims_source_max_chars: cap,
        trust: trust_states,
        policy: run.eval.trust.clone(),
        agreement,
        score_rows_written,
        trust_rows_written,
    })
}

/// Tally `check` on `class` over every group that carries that mutant, whole
/// and split by the original's source length. Both sides are read at the
/// mutant's planted claim, so a per-claim check is scored on the claim the
/// mutation changed and nothing else.
fn tally(
    groups: &[TraceGroup],
    records: &[Vec<TraceRecord>],
    check: &str,
    class: PlantedClass,
    claims_cap: usize,
) -> (ClassTally, SourceBuckets) {
    let mut whole = ClassTally::default();
    let mut buckets = SourceBuckets::default();
    for (group, group_records) in groups.iter().zip(records) {
        let Some(at) = group.classes.iter().position(|c| *c == Some(class)) else {
            continue;
        };
        let claim = group.claims[at];
        let (original, mutant) = (&group_records[0].metrics, &group_records[at].metrics);
        let in_bucket = buckets.of_mut(group.fixtures[0].source.chars().count(), claims_cap);
        if not_judgeable(check, original) || not_judgeable(check, mutant) {
            whole.exclude();
            in_bucket.exclude();
            continue;
        }
        let (original, mutant) = (flags(check, original, claim), flags(check, mutant, claim));
        whole.add(original, mutant);
        in_bucket.add(original, mutant);
    }
    (whole, buckets)
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
