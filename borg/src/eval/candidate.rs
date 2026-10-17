//! Candidate distill patterns: score a proposed pattern against the current
//! one on staged traces, with the coverage floor, and record `candidate` rows.
//!
//! The metric is quote grounding x claim support x listicle survival. A
//! candidate that cuts claims per note or enumeration items per note by more
//! than `eval.candidate.max-coverage-cut` against the baseline scores 0, so a
//! pattern cannot win by saying less ([`objective`]).
//!
//! The search itself lives in the `eval/` uv project (`python -m propose`,
//! GEPA): it proposes pattern text and calls `sb borg eval
//! --candidate-pattern` for every score. This module owns the split, the
//! scoring, the floor, the rows and (in [`file`]) the candidate file and
//! `--apply-candidate`, so the objective has one definition.
//!
//! Each arm (baseline = the current pattern, candidate = the proposal) is a
//! `--regenerate` of the staged sources with only the named pattern swapped:
//! [`CandidateFabric`] points that one pattern at a file and resolves every
//! other pattern as `--patterns-dir` does. Only traces whose distill calls the
//! named pattern are scored; a distill that never called it is an error.

pub mod file;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use distillers::{Dispatch, DistillInputs, FabricCaller, FabricRequest};
use eyre::{Context, Result, bail};
use futures::stream::{self, StreamExt};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use vault::distilled::Distilled;

use super::cache::{JudgmentCache, stable_hash};
use super::judge::{ClaimJudge, ClaimVerdict, DistillationJudge};
use super::regenerate::distill_kind_for_fixture;
use super::report::TraceRecord;
use super::scores::{ScoreStore, rows_from_record};
use super::{EvalOpts, EvalOutcome, Fixture, RUN_KIND_CANDIDATE, evaluate, fixtures, patterns::PatternDirFabric};

/// The distill patterns a candidate may replace, each with the fixture kind
/// whose single-call path uses it. Chunk and reduce patterns are absent: their
/// input is a chunk or a set of partials, never a staged source.
pub const CANDIDATE_PATTERNS: [(&str, &str); 6] = [
    ("distill-video", "video"),
    ("distill-article", "article"),
    ("distill-thread", "thread"),
    ("distill-session", "session"),
    ("distill-voicenote", "voicenote"),
    ("distill-repo", "repo"),
];

/// Characters of a trace's source the JSON score carries for reflection.
const SOURCE_HEAD_CHARS: usize = 2_000;

/// The fixture kind `pattern` distills, or an error naming the patterns a
/// candidate may replace.
pub fn pattern_kind(pattern: &str) -> Result<&'static str> {
    CANDIDATE_PATTERNS
        .iter()
        .find(|(name, _)| *name == pattern)
        .map(|(_, kind)| *kind)
        .ok_or_else(|| {
            let names: Vec<&str> = CANDIDATE_PATTERNS.iter().map(|(n, _)| *n).collect();
            eyre::eyre!(
                "no candidate support for pattern '{pattern}'; one of: {}",
                names.join(", ")
            )
        })
}

/// Does a `kind` source of this length reach the kind's single-call pattern?
/// The repo distiller is always one call; the others switch to chunk/reduce
/// above the same token threshold the distillers use.
pub fn reaches_single_call(kind: &str, source: &str) -> bool {
    kind == "repo" || distillers::parse::approx_tokens(source.len()) <= distillers::video::SINGLE_CALL_TOKEN_THRESHOLD
}

/// `run_id` of a candidate run: the pattern name plus the hash of the
/// pattern text, so two candidates (or a candidate equal to the baseline)
/// never share a row unless their text is identical.
pub fn run_id(pattern: &str, text: &str) -> String {
    format!("{pattern}:{}", stable_hash(text))
}

/// A frozen pair of disjoint samples of traces that reach `pattern`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Split {
    pub pattern: String,
    pub kind: String,
    /// Staged traces of the kind that reach the pattern.
    pub eligible: usize,
    /// The search sample.
    pub train: Vec<String>,
    /// The disjoint re-score sample.
    pub holdout: Vec<String>,
}

/// Split `fixtures` into `n` train and `n` holdout traces that reach
/// `pattern`'s single-call path, ordered by a stable hash of the trace id, so
/// a rerun over the same staging picks the same traces. Fewer than `2n`
/// eligible traces is an error.
pub fn split(fixtures: &[Fixture], pattern: &str, n: usize) -> Result<Split> {
    let kind = pattern_kind(pattern)?;
    if n == 0 {
        bail!("--candidate-split needs at least 1 trace per sample");
    }
    let mut eligible: Vec<&Fixture> = fixtures
        .iter()
        .filter(|f| f.kind == kind && reaches_single_call(kind, &f.source))
        .collect();
    eligible.sort_by_key(|f| (stable_hash(&f.slug), f.slug.clone()));
    if eligible.len() < 2 * n {
        bail!(
            "only {} staged {kind} traces reach {pattern}; two disjoint samples of {n} need {}",
            eligible.len(),
            2 * n
        );
    }
    let ids: Vec<String> = eligible.iter().map(|f| f.slug.clone()).collect();
    Ok(Split {
        pattern: pattern.to_string(),
        kind: kind.to_string(),
        eligible: ids.len(),
        train: ids[..n].to_vec(),
        holdout: ids[n..2 * n].to_vec(),
    })
}

/// [`split`] over borg's staging root.
pub fn load_split(staged_root: &Path, pattern: &str, n: usize) -> Result<Split> {
    log::debug!(
        "eval::candidate::load_split: root={} pattern={pattern} n={n}",
        staged_root.display()
    );
    let kind = pattern_kind(pattern)?;
    let fixtures: Vec<Fixture> = fixtures::load_staged(staged_root, None)?
        .into_iter()
        .filter(|f| f.kind == kind)
        .collect();
    split(&fixtures, pattern, n)
}

/// Load the named traces from staging, in order. A trace that no longer
/// loads (swept, unreadable, an unscored kind) or does not reach `pattern` is
/// an error naming it: a frozen sample with a hole is not the same sample.
pub fn load_traces(staged_root: &Path, pattern: &str, traces: &[String]) -> Result<Vec<Fixture>> {
    let kind = pattern_kind(pattern)?;
    let mut out = Vec::with_capacity(traces.len());
    for trace in traces {
        let fixture = match fixtures::probe_staged_trace(staged_root, trace) {
            fixtures::StagedTrace::Loaded(f) => *f,
            fixtures::StagedTrace::Absent => bail!("trace {trace} is no longer staged under {}", staged_root.display()),
            fixtures::StagedTrace::Unreadable(why) => bail!("trace {trace} is unreadable: {why}"),
        };
        if fixture.kind != kind || !reaches_single_call(kind, &fixture.source) {
            bail!("trace {trace} ({}) does not reach {pattern}", fixture.kind);
        }
        out.push(fixture);
    }
    Ok(out)
}

/// One pattern under test: its text and where it was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arm {
    pub run_id: String,
    pub file: PathBuf,
    pub text: String,
}

impl Arm {
    /// Read `file` as a version of `pattern`.
    pub fn load(pattern: &str, file: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(file).with_context(|| format!("reading pattern {}", file.display()))?;
        if text.trim().is_empty() {
            bail!("pattern {} is empty", file.display());
        }
        Ok(Self {
            run_id: run_id(pattern, &text),
            file: file.to_path_buf(),
            text,
        })
    }
}

/// What one note contributes to the metric.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct NoteCounts {
    pub claims: usize,
    pub enumeration_items: usize,
    pub quoted: usize,
    pub grounded: usize,
    /// Claims with a `judge-claims` verdict, and how many were `supported`.
    pub judged: usize,
    pub supported: usize,
    /// Listicle survival when the note declares a count; `None` otherwise.
    pub listicle: Option<f64>,
}

impl NoteCounts {
    /// Counts for one graded note. `None` when the claim judge produced no
    /// verdicts for it (an error row): an unjudged note has no claim support.
    pub fn of(record: &TraceRecord, distilled: &Distilled) -> Option<Self> {
        let verdicts = record.metrics.judge_claims.as_ref()?;
        let (grounded, quoted) = record
            .metrics
            .quote_grounding
            .map_or((0, 0), |g| (g.grounded, g.quoted));
        Some(Self {
            claims: distilled.claims.len(),
            enumeration_items: distilled.enumeration.as_ref().map_or(0, |e| e.items.len()),
            quoted,
            grounded,
            judged: verdicts.len(),
            supported: verdicts.iter().filter(|v| **v == ClaimVerdict::Supported).count(),
            listicle: record.metrics.listicle_survival,
        })
    }
}

/// Summed counts over a sample of notes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SampleMetrics {
    pub notes: usize,
    pub claims: usize,
    pub enumeration_items: usize,
    pub quoted: usize,
    pub grounded: usize,
    pub judged: usize,
    pub supported: usize,
    pub listicle_sum: f64,
    pub listicle_notes: usize,
}

impl SampleMetrics {
    pub fn of<'a>(notes: impl IntoIterator<Item = &'a NoteCounts>) -> Self {
        let mut m = Self::default();
        for n in notes {
            m.notes += 1;
            m.claims += n.claims;
            m.enumeration_items += n.enumeration_items;
            m.quoted += n.quoted;
            m.grounded += n.grounded;
            m.judged += n.judged;
            m.supported += n.supported;
            if let Some(l) = n.listicle {
                m.listicle_sum += l;
                m.listicle_notes += 1;
            }
        }
        m
    }

    pub fn claims_per_note(&self) -> f64 {
        per_note(self.claims, self.notes)
    }

    pub fn enumeration_items_per_note(&self) -> f64 {
        per_note(self.enumeration_items, self.notes)
    }
}

fn per_note(total: usize, notes: usize) -> f64 {
    if notes == 0 { 0.0 } else { total as f64 / notes as f64 }
}

/// The three factors of the metric.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Factors {
    pub quote_grounding: f64,
    pub claim_support: f64,
    pub listicle_survival: f64,
}

/// The metric for one sample against its baseline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Objective {
    /// The product of the factors, or 0 when the coverage floor is breached.
    pub score: f64,
    pub factors: Factors,
    pub claims_per_note: f64,
    pub enumeration_items_per_note: f64,
    /// Each coverage measure cut by more than the allowed share. Non-empty
    /// means `score` is 0.
    pub floor_breaches: Vec<String>,
}

/// Score `candidate` against `baseline` (pass the baseline as both to get its
/// own score).
///
/// - Quote grounding is grounded/quoted. A sample with no quote scores 1 only
///   when the baseline quoted nothing either, so dropping every quote cannot
///   win.
/// - Claim support is supported/judged claims, 0 with no claim judged.
/// - Listicle survival is the mean over notes that declare a count, 1 when
///   none does.
/// - The floor: claims per note and enumeration items per note may each fall
///   at most `max_cut` (a share) below the baseline; a larger cut scores 0.
pub fn objective(baseline: &SampleMetrics, candidate: &SampleMetrics, max_cut: f64) -> Result<Objective> {
    if candidate.notes == 0 || baseline.notes == 0 {
        bail!("no scored notes: an objective needs at least one note on each side");
    }
    let quote_grounding = if candidate.quoted > 0 {
        candidate.grounded as f64 / candidate.quoted as f64
    } else if baseline.quoted == 0 {
        1.0
    } else {
        0.0
    };
    let claim_support = if candidate.judged > 0 {
        candidate.supported as f64 / candidate.judged as f64
    } else {
        0.0
    };
    let listicle_survival = if candidate.listicle_notes > 0 {
        candidate.listicle_sum / candidate.listicle_notes as f64
    } else {
        1.0
    };
    let factors = Factors {
        quote_grounding,
        claim_support,
        listicle_survival,
    };
    let mut floor_breaches = Vec::new();
    for (label, base, cand) in [
        (
            "claims per note",
            baseline.claims_per_note(),
            candidate.claims_per_note(),
        ),
        (
            "enumeration items per note",
            baseline.enumeration_items_per_note(),
            candidate.enumeration_items_per_note(),
        ),
    ] {
        if base > 0.0 && cand < base * (1.0 - max_cut) - 1e-9 {
            floor_breaches.push(format!(
                "{label} {base:.2} -> {cand:.2} ({:+.0}%) cuts more than {:.0}%",
                (cand / base - 1.0) * 100.0,
                max_cut * 100.0
            ));
        }
    }
    let score = if floor_breaches.is_empty() {
        quote_grounding * claim_support * listicle_survival
    } else {
        0.0
    };
    Ok(Objective {
        score,
        factors,
        claims_per_note: candidate.claims_per_note(),
        enumeration_items_per_note: candidate.enumeration_items_per_note(),
        floor_breaches,
    })
}

/// Distill one fixture with a file standing in for the pattern under test.
#[async_trait]
pub trait PatternDistiller: Send + Sync {
    /// Distill `fixture`'s source with `pattern_file` as the pattern under
    /// test. Fails when the distill never called that pattern.
    async fn distill(&self, fixture: &Fixture, pattern_file: &Path) -> Result<Distilled>;
}

/// A `FabricCaller` that sends `pattern` to `file` and every other pattern
/// through `inner` (the `--patterns-dir` resolution), counting the swaps.
#[derive(Debug, Clone)]
pub struct CandidateFabric<F> {
    pub inner: PatternDirFabric<F>,
    pub pattern: String,
    pub file: PathBuf,
    pub calls: Arc<AtomicUsize>,
}

#[async_trait]
impl<F: FabricCaller> FabricCaller for CandidateFabric<F> {
    async fn call(&self, mut request: FabricRequest) -> Result<String> {
        if request.pattern == self.pattern {
            self.calls.fetch_add(1, Ordering::SeqCst);
            request.pattern = self.file.to_string_lossy().to_string();
        }
        self.inner.call(request).await
    }
}

/// The production [`PatternDistiller`]: borg's fabric-config dispatcher with
/// the pattern under test swapped for a file and every other pattern
/// resolved from `patterns_dir` (deployed when `None`).
pub struct FabricPatternDistiller {
    pub fabric: crate::config::FabricConfig,
    pub patterns_dir: Option<PathBuf>,
    pub pattern: String,
}

#[async_trait]
impl PatternDistiller for FabricPatternDistiller {
    async fn distill(&self, fixture: &Fixture, pattern_file: &Path) -> Result<Distilled> {
        let calls = Arc::new(AtomicUsize::new(0));
        let caller = CandidateFabric {
            inner: PatternDirFabric {
                inner: crate::stages::distill::fabric_shell(&self.fabric),
                dir: self.patterns_dir.clone(),
            },
            pattern: self.pattern.clone(),
            file: pattern_file.to_path_buf(),
            calls: calls.clone(),
        };
        let dispatcher = crate::stages::distill::dispatcher_with_caller(&self.fabric, caller);
        let kind = distill_kind_for_fixture(&fixture.kind)?;
        let distilled = dispatcher
            .distill(
                kind,
                DistillInputs {
                    transcript: &fixture.source,
                    ..Default::default()
                },
            )
            .await?;
        if calls.load(Ordering::SeqCst) == 0 {
            bail!(
                "the distill of {} never called {}: its source took another route",
                fixture.slug,
                self.pattern
            );
        }
        Ok(distilled)
    }
}

const CREATE_DISTILLS: &str = "CREATE TABLE IF NOT EXISTS candidate_distills (
    run_id          TEXT NOT NULL,
    trace           TEXT NOT NULL,
    source_hash     TEXT NOT NULL,
    distill_model   TEXT NOT NULL,
    distilled_yaml  TEXT NOT NULL,
    distilled_at    TEXT NOT NULL,
    PRIMARY KEY (run_id, trace, source_hash, distill_model)
);";

/// Distills of a pattern version over a source, in `eval-cache.db`. A search
/// scores the same pattern on the same trace many times (and the baseline on
/// every call); the distill is the expensive part, so it is paid once.
pub struct DistillCache {
    conn: Connection,
}

impl DistillCache {
    pub fn open(path: &Path) -> Result<Self> {
        log::debug!("DistillCache::open: path={}", path.display());
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("creating eval cache dir {}", parent.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening distill cache {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .context("setting distill cache busy timeout")?;
        conn.execute_batch(CREATE_DISTILLS)
            .context("creating candidate_distills table")?;
        Ok(Self { conn })
    }

    /// An in-memory cache (tests).
    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("opening in-memory distill cache")?;
        conn.execute_batch(CREATE_DISTILLS)
            .context("creating candidate_distills table")?;
        Ok(Self { conn })
    }

    pub fn get(&self, run_id: &str, fixture: &Fixture, model: &str) -> Result<Option<Distilled>> {
        let yaml: Option<String> = self
            .conn
            .query_row(
                "SELECT distilled_yaml FROM candidate_distills
                 WHERE run_id = ?1 AND trace = ?2 AND source_hash = ?3 AND distill_model = ?4",
                params![run_id, fixture.slug, stable_hash(&fixture.source), model],
                |r| r.get(0),
            )
            .optional()
            .context("reading a cached candidate distill")?;
        yaml.map(|y| serde_yaml::from_str(&y).context("parsing a cached candidate distill"))
            .transpose()
    }

    pub fn put(&self, run_id: &str, fixture: &Fixture, model: &str, distilled: &Distilled, at: &str) -> Result<()> {
        let yaml = serde_yaml::to_string(distilled).context("serializing a candidate distill")?;
        self.conn
            .execute(
                "INSERT OR REPLACE INTO candidate_distills
                 (run_id, trace, source_hash, distill_model, distilled_yaml, distilled_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![run_id, fixture.slug, stable_hash(&fixture.source), model, yaml, at],
            )
            .context("writing a candidate distill")?;
        Ok(())
    }
}

/// One `--candidate-pattern` scoring request.
pub struct ScoreRequest<'a> {
    pub pattern: &'a str,
    pub baseline: &'a Arm,
    pub candidate: &'a Arm,
    pub fixtures: &'a [Fixture],
    /// The distiller's model, part of the distill cache key.
    pub distill_model: &'a str,
    pub max_coverage_cut: f64,
    pub distill_workers: usize,
    /// Re-distill even when a cached distill exists (`--apply-candidate`'s
    /// confirmation). The fresh distill still replaces the cached one.
    pub fresh: bool,
    pub scored_at: &'a str,
}

/// The judges and stores a scoring run grades and records through.
pub struct Grading<'a> {
    pub judge: &'a dyn DistillationJudge,
    pub claim_judge: &'a dyn ClaimJudge,
    pub judgments: &'a JudgmentCache,
    pub distills: &'a DistillCache,
    pub scores: &'a ScoreStore,
    /// `judge_model` and `patterns_dir` of the judges; `no_judge` must be off.
    pub opts: &'a EvalOpts,
}

/// One trace under both arms.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct TraceScore {
    pub trace: String,
    pub baseline: Option<NoteCounts>,
    pub candidate: Option<NoteCounts>,
    /// This note's factor product, 0 when the scored sample as a whole
    /// breaches the coverage floor; `None` when either arm errored.
    pub score: Option<f64>,
    /// Coverage cuts on THIS note against its baseline note: feedback, not
    /// a zero (the floor is a per-note average over the sample).
    pub floor_breaches: Vec<String>,
    /// Distill and judge failures on either arm, labelled by arm.
    pub errors: Vec<String>,
    /// What the candidate note got wrong, for the search's reflection step.
    pub feedback: Vec<String>,
    /// The candidate note as the judge saw it.
    pub candidate_note: String,
    pub source_head: String,
}

/// Both arms over one sample.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct SampleScore {
    pub pattern: String,
    pub judge_model: String,
    pub baseline_run_id: String,
    pub candidate_run_id: String,
    pub traces: Vec<TraceScore>,
    /// Every trace scored on both arms. The sample objectives exist only
    /// then: a sample with a hole is not the sample.
    pub complete: bool,
    pub baseline: Option<Objective>,
    pub candidate: Option<Objective>,
}

impl SampleScore {
    /// Does the candidate beat the baseline, floor intact?
    pub fn candidate_wins(&self) -> bool {
        match (&self.baseline, &self.candidate) {
            (Some(b), Some(c)) => c.floor_breaches.is_empty() && c.score > b.score,
            _ => false,
        }
    }

    /// Plain-text summary for a human.
    pub fn render(&self) -> String {
        let mut o = format!(
            "candidate {}  vs  baseline {}  ({} traces, judge {})\n",
            self.candidate_run_id,
            self.baseline_run_id,
            self.traces.len(),
            self.judge_model
        );
        o.push_str(&format!(
            "{:<16} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9}\n",
            "trace", "base", "cand", "claims", "c-claims", "items", "c-items"
        ));
        for t in &self.traces {
            let cell = |v: Option<f64>| v.map_or_else(|| "-".to_string(), |v| format!("{v:.3}"));
            let base_score = t.baseline.and_then(|b| objective_of_one(&b, &b));
            o.push_str(&format!(
                "{:<16} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9}\n",
                t.trace,
                cell(base_score),
                cell(t.score),
                t.baseline.map_or("-".to_string(), |b| b.claims.to_string()),
                t.candidate.map_or("-".to_string(), |c| c.claims.to_string()),
                t.baseline.map_or("-".to_string(), |b| b.enumeration_items.to_string()),
                t.candidate.map_or("-".to_string(), |c| c.enumeration_items.to_string()),
            ));
            for e in &t.errors {
                o.push_str(&format!("    error: {e}\n"));
            }
            for b in &t.floor_breaches {
                o.push_str(&format!("    floor: {b}\n"));
            }
        }
        match (&self.baseline, &self.candidate) {
            (Some(b), Some(c)) => {
                o.push_str(&format!("baseline  {}\n", render_objective(b)));
                o.push_str(&format!("candidate {}\n", render_objective(c)));
                o.push_str(if self.candidate_wins() {
                    "verdict: the candidate beats the baseline\n"
                } else {
                    "verdict: the candidate does not beat the baseline\n"
                });
            }
            _ => o.push_str("verdict: INCOMPLETE - a trace errored on an arm; no sample objective\n"),
        }
        o
    }
}

/// One-line rendering of an objective.
pub fn render_objective(o: &Objective) -> String {
    let mut s = format!(
        "score {:.4} = quote grounding {:.3} x claim support {:.3} x listicle survival {:.3}; claims/note {:.2}, enumeration items/note {:.2}",
        o.score,
        o.factors.quote_grounding,
        o.factors.claim_support,
        o.factors.listicle_survival,
        o.claims_per_note,
        o.enumeration_items_per_note
    );
    for b in &o.floor_breaches {
        s.push_str(&format!("; FLOOR: {b}"));
    }
    s
}

fn objective_of_one(baseline: &NoteCounts, candidate: &NoteCounts) -> Option<f64> {
    let b = SampleMetrics::of([baseline]);
    let c = SampleMetrics::of([candidate]);
    objective(&b, &c, 0.0).ok().map(|o| o.score)
}

/// Score `req.candidate` against `req.baseline` on `req.fixtures`: distill
/// each arm (cache first, `distill_workers` at a time), grade both with the
/// judges, write every graded note's rows as `candidate` rows under its
/// arm's `run_id`, and compute the per-trace and sample metrics.
pub async fn score(req: &ScoreRequest<'_>, distiller: &dyn PatternDistiller, g: &Grading<'_>) -> Result<SampleScore> {
    log::debug!(
        "eval::candidate::score: pattern={} baseline={} candidate={} traces={} fresh={}",
        req.pattern,
        req.baseline.run_id,
        req.candidate.run_id,
        req.fixtures.len(),
        req.fresh
    );
    if g.opts.no_judge {
        bail!("a candidate is scored with the claim judge; drop --no-judge");
    }
    let arms = [req.baseline, req.candidate];
    // A candidate identical to the baseline (the search's seed) is one
    // pattern: distill it once and grade the same note on both arms.
    let same_pattern = req.baseline.run_id == req.candidate.run_id;
    let distilled_arms = if same_pattern { 1 } else { 2 };
    // (arm index, fixture index) -> distill result
    let mut distilled: BTreeMap<(usize, usize), Result<Distilled>> = BTreeMap::new();
    let mut misses: Vec<(usize, usize)> = Vec::new();
    for (a, arm) in arms.iter().enumerate().take(distilled_arms) {
        for (f, fx) in req.fixtures.iter().enumerate() {
            let cached = if req.fresh {
                None
            } else {
                g.distills.get(&arm.run_id, fx, req.distill_model)?
            };
            match cached {
                Some(d) => {
                    distilled.insert((a, f), Ok(d));
                }
                None => misses.push((a, f)),
            }
        }
    }
    let fresh: Vec<((usize, usize), Result<Distilled>)> = stream::iter(misses)
        .map(|(a, f)| async move {
            let result = distiller.distill(&req.fixtures[f], &arms[a].file).await;
            ((a, f), result)
        })
        .buffer_unordered(req.distill_workers.max(1))
        .collect()
        .await;
    for ((a, f), result) in fresh {
        if let Ok(d) = &result
            && d.meta.validation.fallback_reason.is_none()
        {
            g.distills
                .put(&arms[a].run_id, &req.fixtures[f], req.distill_model, d, req.scored_at)?;
        }
        distilled.insert((a, f), result);
    }
    if same_pattern {
        for f in 0..req.fixtures.len() {
            let copy = match distilled.get(&(0, f)) {
                Some(Ok(d)) => Ok(d.clone()),
                Some(Err(e)) => Err(eyre::eyre!("{e:#}")),
                None => continue,
            };
            distilled.insert((1, f), copy);
        }
    }

    // Grade each arm's successful distills.
    let mut graded: BTreeMap<(usize, usize), (TraceRecord, Distilled)> = BTreeMap::new();
    let mut errors: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for (a, arm) in arms.iter().enumerate() {
        let label = if a == 0 { "baseline" } else { "candidate" };
        let mut set = Vec::new();
        let mut index = Vec::new();
        for (f, fx) in req.fixtures.iter().enumerate() {
            match distilled.remove(&(a, f)) {
                Some(Ok(d)) => {
                    index.push(f);
                    set.push(Fixture {
                        distilled: d,
                        capture_note: None,
                        ..fx.clone()
                    });
                }
                Some(Err(e)) => errors.entry(f).or_default().push(format!("{label} distill: {e:#}")),
                None => errors.entry(f).or_default().push(format!("{label} distill: missing")),
            }
        }
        if set.is_empty() {
            continue;
        }
        let EvalOutcome::Report(report) =
            evaluate(&set, &BTreeMap::new(), g.judgments, g.judge, g.claim_judge, g.opts)?
        else {
            bail!("evaluate returned a calibration sheet while scoring a candidate");
        };
        for ((f, fx), mut record) in index.into_iter().zip(set).zip(report.traces) {
            record.run_kind = RUN_KIND_CANDIDATE.to_string();
            record.run_id = arm.run_id.clone();
            for row in rows_from_record(&record, true, req.scored_at) {
                g.scores.put(&row)?;
            }
            for e in &record.errors {
                errors
                    .entry(f)
                    .or_default()
                    .push(format!("{label} {}: {}", e.check, e.error));
            }
            graded.insert((a, f), (record, fx.distilled));
        }
    }

    let mut traces = Vec::with_capacity(req.fixtures.len());
    let (mut base_counts, mut cand_counts) = (Vec::new(), Vec::new());
    for (f, fx) in req.fixtures.iter().enumerate() {
        let counts = |a: usize| graded.get(&(a, f)).and_then(|(r, d)| NoteCounts::of(r, d));
        let (baseline, candidate) = (counts(0), counts(1));
        let trace_errors = errors.remove(&f).unwrap_or_default();
        // A trace's own score is its factor product; the coverage floor is a
        // per-note AVERAGE over the sample (applied below), so one note that
        // lost two claims is a finding, not a zero.
        let (score, floor_breaches) = match (&baseline, &candidate) {
            (Some(b), Some(c)) => {
                let o = objective(&SampleMetrics::of([b]), &SampleMetrics::of([c]), req.max_coverage_cut)?;
                base_counts.push(*b);
                cand_counts.push(*c);
                let f = o.factors;
                (
                    Some(f.quote_grounding * f.claim_support * f.listicle_survival),
                    o.floor_breaches,
                )
            }
            _ => (None, Vec::new()),
        };
        let mut feedback = graded.get(&(1, f)).map(|(r, d)| feedback_for(r, d)).unwrap_or_default();
        feedback.extend(floor_breaches.iter().map(|b| format!("this note: {b}")));
        traces.push(TraceScore {
            trace: fx.slug.clone(),
            baseline,
            candidate,
            score,
            floor_breaches,
            errors: trace_errors,
            feedback,
            candidate_note: graded
                .get(&(1, f))
                .map(|(r, _)| r.note_text.clone())
                .unwrap_or_default(),
            source_head: fx.source.chars().take(SOURCE_HEAD_CHARS).collect(),
        });
    }
    // The floor over every trace scored on both arms: a breach zeroes every
    // trace's score, so a search summing per-trace scores sees the same
    // floor the sample objective applies.
    if !cand_counts.is_empty() {
        let floor = objective(
            &SampleMetrics::of(&base_counts),
            &SampleMetrics::of(&cand_counts),
            req.max_coverage_cut,
        )?;
        if !floor.floor_breaches.is_empty() {
            for t in traces.iter_mut().filter(|t| t.score.is_some()) {
                t.score = Some(0.0);
                t.feedback.extend(
                    floor
                        .floor_breaches
                        .iter()
                        .map(|b| format!("coverage floor (sample): {b}")),
                );
            }
        }
    }
    let complete = !traces.is_empty() && traces.iter().all(|t| t.score.is_some());
    let (baseline, candidate) = if complete {
        let b = SampleMetrics::of(&base_counts);
        let c = SampleMetrics::of(&cand_counts);
        (
            Some(objective(&b, &b, req.max_coverage_cut)?),
            Some(objective(&b, &c, req.max_coverage_cut)?),
        )
    } else {
        (None, None)
    };
    Ok(SampleScore {
        pattern: req.pattern.to_string(),
        judge_model: g.opts.judge_model.clone(),
        baseline_run_id: req.baseline.run_id.clone(),
        candidate_run_id: req.candidate.run_id.clone(),
        traces,
        complete,
        baseline,
        candidate,
    })
}

/// What a graded candidate note got wrong, one line per finding.
fn feedback_for(record: &TraceRecord, distilled: &Distilled) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(reason) = &distilled.meta.validation.fallback_reason {
        out.push(format!("the output did not parse and fell back: {reason}"));
    }
    if let Some(verdicts) = &record.metrics.judge_claims {
        for (claim, verdict) in record.claims.iter().zip(verdicts) {
            if *verdict != ClaimVerdict::Supported {
                out.push(format!(
                    "claim {} judged {}: {}",
                    claim.id,
                    verdict_label(*verdict),
                    claim.text
                ));
            }
        }
    }
    if let Some(g) = record.metrics.quote_grounding
        && g.grounded < g.quoted
    {
        out.push(format!(
            "{} of {} quotes are not verbatim in the source",
            g.quoted - g.grounded,
            g.quoted
        ));
    }
    if let Some(l) = record.metrics.listicle_survival
        && l < 1.0
    {
        out.push(format!(
            "the enumeration lists fewer items than it declares (survival {l:.2})"
        ));
    }
    for reason in &record.metrics.placeholder_artifacts {
        out.push(format!("placeholder filler: {reason}"));
    }
    out
}

fn verdict_label(v: ClaimVerdict) -> &'static str {
    match v {
        ClaimVerdict::Supported => "supported",
        ClaimVerdict::Unsupported => "unsupported",
        ClaimVerdict::Contradicted => "contradicted",
    }
}

#[cfg(test)]
mod tests;
