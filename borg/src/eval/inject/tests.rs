use std::collections::HashSet;

use super::*;
use crate::eval::calc::quote_grounding;
use crate::eval::judge::{AxisScores, ClaimVerdict, MockClaimJudge, MockJudge};
use crate::eval::scores::{DETERMINISTIC_CHECKS, ScoreRow};
use crate::eval::{RUN_KIND_BASELINE, judge_note_text};
use vault::distilled::Claim;

const MODEL: &str = "gemini-2.5-pro";

fn golden_fixtures() -> Vec<Fixture> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../config/eval/distill-fixtures");
    crate::eval::load(&dir).unwrap()
}

fn claim(text: &str, who: Option<&str>, quote: Option<&str>) -> Claim {
    Claim {
        text: text.to_string(),
        who: who.map(str::to_string),
        quote: quote.map(str::to_string),
        ..Default::default()
    }
}

/// A note every class applies to, with a source that backs it.
fn fixture(trace: &str) -> Fixture {
    let source = format!(
        "In the {trace} talk, Ada Lovelace said the engine computes 7 results per cycle. \
         She added that the cache doubles throughput on large batches. \
         Grace Hopper argued that compilers make programs portable across machines."
    );
    let distilled = Distilled {
        summary: format!("A talk ({trace}) on engines and compilers."),
        claims: vec![
            claim(
                "The engine computes 7 results per cycle.",
                Some("Ada Lovelace"),
                Some("the engine computes 7 results per cycle"),
            ),
            claim("The cache doubles throughput on large batches.", None, None),
            claim(
                "Compilers make programs portable.",
                Some("Grace Hopper"),
                Some("compilers make programs portable across machines"),
            ),
        ],
        ..Default::default()
    };
    Fixture {
        id: format!("article/{trace}"),
        kind: "article".to_string(),
        slug: trace.to_string(),
        source,
        distilled,
        capture_note: None,
    }
}

fn no_judge_opts() -> EvalOpts {
    EvalOpts {
        no_judge: true,
        ..EvalOpts::default()
    }
}

#[test]
fn quote_grounding_catches_every_fabricated_quote_with_zero_llm_calls() {
    let mut fixtures = golden_fixtures();
    fixtures.extend((0..5).map(|i| fixture(&format!("t{i}"))));
    let mutants: Vec<Fixture> = fixtures
        .iter()
        .filter_map(|fx| {
            plant(PlantedClass::FabricatedQuote, &fx.distilled, &fx.source).map(|planted| Fixture {
                distilled: planted.distilled,
                ..fx.clone()
            })
        })
        .collect();
    assert!(
        mutants.len() >= 8,
        "only {} fixtures took a fabricated quote",
        mutants.len()
    );

    let judge = MockJudge::new(AxisScores {
        claim_coverage: 3,
        anchor_validity: 3,
        summary_faithfulness: 3,
    });
    let claim_judge = MockClaimJudge::new(ClaimVerdict::Supported);
    let cache = cache::JudgmentCache::open_memory().unwrap();
    let EvalOutcome::Report(report) = evaluate(
        &mutants,
        &BTreeMap::new(),
        &cache,
        &judge,
        &claim_judge,
        &no_judge_opts(),
    )
    .unwrap() else {
        panic!("expected a report");
    };
    assert_eq!(judge.calls(), 0);
    assert_eq!(claim_judge.calls(), 0);
    for (mutant, record) in mutants.iter().zip(&report.traces) {
        let quote = mutant.distilled.claims[0].quote.as_deref().unwrap();
        let source_words: HashSet<String> = grounding_words(&mutant.source).into_iter().collect();
        assert!(
            grounding_words(quote).iter().all(|w| !source_words.contains(w)),
            "{}: fabricated quote reuses a source word: {quote}",
            mutant.id
        );
        assert_eq!(
            flags(CHECK_QUOTE_GROUNDING, &record.metrics, Some(0)),
            Some(true),
            "{} not caught",
            mutant.id
        );
    }
}

#[test]
fn a_fabricated_quote_uses_only_words_absent_from_the_source_and_skips_present_ones() {
    let mut fx = fixture("t");
    fx.source.push_str(" marmalade trombone porcupine");
    let planted = plant(PlantedClass::FabricatedQuote, &fx.distilled, &fx.source).unwrap();
    assert_eq!(planted.claim, Some(0));
    let mutant = planted.distilled;
    let quote = mutant.claims[0].quote.clone().unwrap();
    assert_eq!(quote, "saxophone tapestry walrus cinnamon hammock pelican");
    assert_eq!(quote_grounding(&fx.source, &mutant), (1, 2));
    assert_eq!(quote_grounding(&fx.source, &fx.distilled), (2, 2));
}

#[test]
fn a_number_swap_picks_a_number_the_source_never_states() {
    let mut fx = fixture("t");
    fx.source.push_str(" It needs 8 cores.");
    let planted = plant(PlantedClass::NumberSwap, &fx.distilled, &fx.source).unwrap();
    assert_eq!(planted.claim, Some(0));
    let mutant = planted.distilled;
    assert_eq!(mutant.claims[0].text, "The engine computes 9 results per cycle.");
    assert_eq!(mutant.claims[0].quote, fx.distilled.claims[0].quote);

    // The swap lands on the first claim that carries a number, and reports it.
    let mut later = fx.distilled.clone();
    later.claims[0].text = "No figures here.".to_string();
    later.claims[2].text = "It runs on 3 machines.".to_string();
    let planted = plant(PlantedClass::NumberSwap, &later, &fx.source).unwrap();
    assert_eq!(planted.claim, Some(2));
    assert_eq!(planted.distilled.claims[2].text, "It runs on 4 machines.");

    let mut no_number = fx.distilled.clone();
    for c in &mut no_number.claims {
        c.text = "No figures here, nor in x86 or H100.".to_string();
    }
    assert!(plant(PlantedClass::NumberSwap, &no_number, &fx.source).is_none());
}

#[test]
fn a_dropped_top_claim_needs_two_claims() {
    let fx = fixture("t");
    let planted = plant(PlantedClass::DroppedTopClaim, &fx.distilled, &fx.source).unwrap();
    assert_eq!(planted.claim, None, "a dropped claim has no index to pair on");
    let mutant = planted.distilled;
    assert_eq!(mutant.claims.len(), 2);
    assert_eq!(mutant.claims[0].text, fx.distilled.claims[1].text);

    let mut one = fx.distilled.clone();
    one.claims.truncate(1);
    assert!(plant(PlantedClass::DroppedTopClaim, &one, &fx.source).is_none());
}

#[test]
fn a_who_change_names_someone_the_source_never_mentions() {
    let fx = fixture("t");
    let planted = plant(PlantedClass::WhoChange, &fx.distilled, &fx.source).unwrap();
    assert_eq!(planted.claim, Some(0));
    let mutant = planted.distilled;
    assert_eq!(mutant.claims[0].who.as_deref(), Some("Dr. Imogen Thistlewood"));
    assert_eq!(mutant.claims[2].who, fx.distilled.claims[2].who);

    let mut no_who = fx.distilled.clone();
    for c in &mut no_who.claims {
        c.who = Some("  ".to_string());
    }
    assert!(plant(PlantedClass::WhoChange, &no_who, &fx.source).is_none());
}

#[test]
fn no_class_applies_to_a_note_with_no_claims() {
    let mut fx = fixture("t");
    fx.distilled.claims.clear();
    for class in PlantedClass::ALL {
        assert!(plant(class, &fx.distilled, &fx.source).is_none(), "{class}");
    }
}

#[test]
fn the_applicability_matrix_matches_the_design() {
    use PlantedClass::*;
    let row = |check: &str| PlantedClass::ALL.map(|c| applies(check, c));
    assert_eq!(row(CHECK_JUDGE_DISTILLATION), [true, true, true, true]);
    assert_eq!(row(CHECK_JUDGE_CLAIMS), [true, false, false, false]);
    assert_eq!(row(CHECK_QUOTE_GROUNDING), [false, true, false, false]);
    assert_eq!(applicable_classes(CHECK_JUDGE_CLAIMS), vec![NumberSwap]);
    assert!(applicable_classes("no-such-check").is_empty());
}

#[test]
fn an_unscored_side_is_an_error_never_a_pass() {
    let mut t = ClassTally::default();
    t.add(None, Some(true));
    t.add(Some(false), None);
    assert_eq!(
        t,
        ClassTally {
            errors: 2,
            ..Default::default()
        }
    );
    assert_eq!(t.paired_catch_rate(), 0.0);
}

#[test]
fn judge_claims_flags_read_the_planted_claim_and_a_missing_verdict_is_an_error() {
    use ClaimVerdict::*;
    let metrics = TraceMetrics {
        judge_claims: Some(vec![Supported, Unsupported, Supported]),
        ..Default::default()
    };
    assert_eq!(flags(CHECK_JUDGE_CLAIMS, &metrics, Some(0)), Some(false));
    assert_eq!(flags(CHECK_JUDGE_CLAIMS, &metrics, Some(1)), Some(true));
    assert_eq!(
        flags(CHECK_JUDGE_CLAIMS, &metrics, None),
        Some(true),
        "note-level: any claim"
    );
    assert_eq!(flags(CHECK_JUDGE_CLAIMS, &metrics, Some(3)), None, "no verdict at 3");
    let unjudged = TraceMetrics::default();
    assert_eq!(flags(CHECK_JUDGE_CLAIMS, &unjudged, Some(0)), None);
}

/// Supports every claim except the one named: a judge that is right about the
/// planted claim and wrong (or the distiller wrong) somewhere else on the note.
struct OneClaimFlaggedJudge {
    flagged: String,
    /// Claims that are not in the originals: the planted ones.
    originals: HashSet<String>,
}

impl ClaimJudge for OneClaimFlaggedJudge {
    fn judge_claims(&self, _kind: &str, _source: &str, claims: &[String]) -> Result<Vec<ClaimVerdict>> {
        Ok(claims
            .iter()
            .map(|c| {
                if *c == self.flagged || !self.originals.contains(c) {
                    ClaimVerdict::Unsupported
                } else {
                    ClaimVerdict::Supported
                }
            })
            .collect())
    }
}

/// The 2026-10-18 measured shape: the judge catches every planted swap, and
/// the original carries another claim it flags. Note-level pairing scored
/// that 0; the metric reads the planted claim, so it scores 1.
#[test]
fn judge_claims_paired_catch_is_on_the_planted_claim_not_the_whole_note() {
    let fixtures: Vec<Fixture> = (0..3).map(|i| fixture(&format!("t{i}"))).collect();
    let originals: HashSet<String> = fixtures
        .iter()
        .flat_map(|f| f.distilled.claims.iter().map(|c| c.text.trim().to_string()))
        .collect();
    let flagged = fixtures[0].distilled.claims[1].text.trim().to_string();
    let factory = |_: &EvalOpts| -> Result<(Box<dyn DistillationJudge>, Box<dyn ClaimJudge>)> {
        Ok((
            Box::new(MockJudge::new(AxisScores {
                claim_coverage: 3,
                anchor_validity: 3,
                summary_faithfulness: 3,
            })),
            Box::new(OneClaimFlaggedJudge {
                flagged: flagged.clone(),
                originals: originals.clone(),
            }),
        ))
    };
    let env = env();
    let config = eval_config(3);
    let opts = EvalOpts::default();
    let run = InjectRun {
        opts: &opts,
        eval: &config,
        cache_path: &env.cache_path,
        now: now(),
    };
    let report = run_inject(&fixtures, &run, &env.scores, &env.trust, &factory).unwrap();
    let t = cell(&report, MODEL, CHECK_JUDGE_CLAIMS, PlantedClass::NumberSwap)
        .tally
        .unwrap();
    // Every fixture's claim 1 ("The cache doubles throughput...") is flagged on
    // original and mutant alike; the swap is on claim 0 and is caught cleanly.
    assert_eq!(
        (t.n, t.paired_caught, t.raw_caught, t.originals_flagged, t.errors),
        (3, 3, 3, 0, 0)
    );
    assert_eq!(t.paired_catch_rate(), 1.0);
    let claims_trust = report
        .trust
        .iter()
        .find(|c| c.judge_model == MODEL && c.check == CHECK_JUDGE_CLAIMS)
        .unwrap();
    assert_eq!(claims_trust.state, TrustState::Trusted);

    // The negative: a judge that flags the planted claim on the original too
    // (here: every claim) is still paired 0 on it.
    let all_flagged = |_: &EvalOpts| -> Result<(Box<dyn DistillationJudge>, Box<dyn ClaimJudge>)> {
        Ok((
            Box::new(MockJudge::new(AxisScores {
                claim_coverage: 3,
                anchor_validity: 3,
                summary_faithfulness: 3,
            })),
            Box::new(MockClaimJudge::new(ClaimVerdict::Unsupported)),
        ))
    };
    let env = self::env();
    let run = InjectRun {
        opts: &opts,
        eval: &config,
        cache_path: &env.cache_path,
        now: now(),
    };
    let report = run_inject(&fixtures, &run, &env.scores, &env.trust, &all_flagged).unwrap();
    let t = cell(&report, MODEL, CHECK_JUDGE_CLAIMS, PlantedClass::NumberSwap)
        .tally
        .unwrap();
    assert_eq!((t.n, t.paired_caught, t.originals_flagged), (3, 0, 3));
}

/// Scores a note perfectly only when it is one of the originals.
struct KnownNotesJudge {
    originals: HashSet<String>,
}

impl DistillationJudge for KnownNotesJudge {
    fn judge(&self, _kind: &str, _source: &str, note: &str) -> Result<AxisScores> {
        let faithful = if self.originals.contains(note) { 3 } else { 2 };
        Ok(AxisScores {
            claim_coverage: faithful,
            anchor_validity: 3,
            summary_faithfulness: 3,
        })
    }
}

/// Supports a claim only when it is one of the originals' claims.
struct KnownClaimsJudge {
    claims: HashSet<String>,
}

impl ClaimJudge for KnownClaimsJudge {
    fn judge_claims(&self, _kind: &str, _source: &str, claims: &[String]) -> Result<Vec<ClaimVerdict>> {
        Ok(claims
            .iter()
            .map(|c| {
                if self.claims.contains(c) {
                    ClaimVerdict::Supported
                } else {
                    ClaimVerdict::Contradicted
                }
            })
            .collect())
    }
}

struct Env {
    _tmp: tempfile::TempDir,
    cache_path: std::path::PathBuf,
    scores: ScoreStore,
    trust: TrustStore,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let cache_path = tmp.path().join("eval-cache.db");
    Env {
        scores: ScoreStore::open(&cache_path).unwrap(),
        trust: TrustStore::open(&cache_path).unwrap(),
        cache_path,
        _tmp: tmp,
    }
}

fn eval_config(min_samples: usize) -> EvalConfig {
    EvalConfig {
        judge_models: vec![MODEL.to_string()],
        trust: TrustConfig {
            min_paired_catch_rate: 0.80,
            min_samples,
        },
        inject_workers: 2,
        ..EvalConfig::default()
    }
}

fn now() -> DateTime<Utc> {
    "2026-10-17T12:00:00Z".parse().unwrap()
}

fn cell<'a>(report: &'a InjectReport, judge: &str, check: &str, class: PlantedClass) -> &'a InjectCell {
    report
        .cells
        .iter()
        .find(|c| c.judge_model == judge && c.check == check && c.class == class)
        .unwrap()
}

#[test]
fn run_inject_measures_a_discerning_judge_and_writes_one_trust_row_per_judge_check_and_class() {
    let fixtures: Vec<Fixture> = (0..3).map(|i| fixture(&format!("t{i}"))).collect();
    let originals: HashSet<String> = fixtures.iter().map(|f| judge_note_text(&f.distilled)).collect();
    let claims: HashSet<String> = fixtures
        .iter()
        .flat_map(|f| f.distilled.claims.iter().map(|c| c.text.trim().to_string()))
        .collect();
    let factory = |_: &EvalOpts| -> Result<(Box<dyn DistillationJudge>, Box<dyn ClaimJudge>)> {
        Ok((
            Box::new(KnownNotesJudge {
                originals: originals.clone(),
            }),
            Box::new(KnownClaimsJudge { claims: claims.clone() }),
        ))
    };
    let env = env();
    let config = eval_config(3);
    let opts = EvalOpts::default();
    let run = InjectRun {
        opts: &opts,
        eval: &config,
        cache_path: &env.cache_path,
        now: now(),
    };
    let report = run_inject(&fixtures, &run, &env.scores, &env.trust, &factory).unwrap();

    assert_eq!(report.run_id, "inject-20261017T120000Z");
    assert_eq!(report.traces, 3);
    assert_eq!(report.cells.len(), 12);
    for c in &report.cells {
        assert_eq!(c.tally.is_some(), applies(c.check, c.class), "{c:?}");
    }
    for class in PlantedClass::ALL {
        let t = cell(&report, MODEL, CHECK_JUDGE_DISTILLATION, class).tally.unwrap();
        assert_eq!((t.n, t.paired_caught, t.originals_flagged), (3, 3, 0), "{class}");
    }
    let claims_swap = cell(&report, MODEL, CHECK_JUDGE_CLAIMS, PlantedClass::NumberSwap)
        .tally
        .unwrap();
    assert_eq!(claims_swap.paired_catch_rate(), 1.0);
    let grounding = cell(&report, "", CHECK_QUOTE_GROUNDING, PlantedClass::FabricatedQuote)
        .tally
        .unwrap();
    assert_eq!((grounding.n, grounding.paired_caught), (3, 3));

    // judge-distillation x4 + judge-claims x1 + quote-grounding x1.
    let rows = env.trust.all().unwrap();
    assert_eq!(report.trust_rows_written, 6);
    assert_eq!(rows.len(), 6);
    assert!(rows.iter().all(|r| {
        applies(
            &r.check_name,
            PlantedClass::ALL
                .into_iter()
                .find(|c| c.as_str() == r.planted_class)
                .unwrap(),
        )
    }));
    let state = |check: &str| {
        report
            .trust
            .iter()
            .find(|t| t.check == check)
            .map(|t| t.state.clone())
            .unwrap()
    };
    assert_eq!(state(CHECK_JUDGE_DISTILLATION), TrustState::Trusted);
    assert_eq!(state(CHECK_JUDGE_CLAIMS), TrustState::Trusted);
    assert_eq!(state(CHECK_QUOTE_GROUNDING), TrustState::Ungated);

    // Originals are baseline rows; each mutant is an inject row under its own run id.
    let score_rows = env.scores.rows_for("t0").unwrap();
    let per_variant = DETERMINISTIC_CHECKS.len() + 2;
    assert_eq!(score_rows.len(), 5 * per_variant);
    assert_eq!(report.score_rows_written, 3 * 5 * per_variant);
    let baseline = score_rows.iter().filter(|r| r.run_kind == RUN_KIND_BASELINE).count();
    assert_eq!(baseline, per_variant);
    let run_ids: HashSet<&str> = score_rows
        .iter()
        .filter(|r| r.run_kind == RUN_KIND_INJECT)
        .map(|r| r.run_id.as_str())
        .collect();
    let expected: HashSet<String> = PlantedClass::ALL
        .iter()
        .map(|c| format!("inject-20261017T120000Z:{c}"))
        .collect();
    assert_eq!(run_ids, expected.iter().map(String::as_str).collect());

    let text = report.render();
    assert!(text.contains("What this misses:"), "{text}");
    assert!(text.contains("the base rate of errors in production"), "{text}");
    assert!(text.contains("Kappa is agreement, not correctness."), "{text}");
    let na_rows = text.lines().filter(|l| l.contains(" n/a ")).count();
    assert_eq!(na_rows, 6, "{text}");
    for column in ["paired", "wilson-lb", "raw", "flag", "errors"] {
        assert!(text.contains(column), "missing column {column}: {text}");
    }
}

#[test]
fn a_judge_that_flags_everything_scores_paired_zero_and_is_untrusted() {
    let fixtures: Vec<Fixture> = (0..3).map(|i| fixture(&format!("t{i}"))).collect();
    let factory = |_: &EvalOpts| -> Result<(Box<dyn DistillationJudge>, Box<dyn ClaimJudge>)> {
        Ok((
            Box::new(MockJudge::new(AxisScores {
                claim_coverage: 1,
                anchor_validity: 3,
                summary_faithfulness: 3,
            })),
            Box::new(MockClaimJudge::new(ClaimVerdict::Unsupported)),
        ))
    };
    let env = env();
    let config = eval_config(3);
    let opts = EvalOpts::default();
    let run = InjectRun {
        opts: &opts,
        eval: &config,
        cache_path: &env.cache_path,
        now: now(),
    };
    let report = run_inject(&fixtures, &run, &env.scores, &env.trust, &factory).unwrap();
    let t = cell(&report, MODEL, CHECK_JUDGE_DISTILLATION, PlantedClass::NumberSwap)
        .tally
        .unwrap();
    assert_eq!(
        (t.raw_catch_rate(), t.flag_rate(), t.paired_catch_rate()),
        (1.0, 1.0, 0.0)
    );
    for t in report.trust.iter().filter(|t| !t.judge_model.is_empty()) {
        assert!(matches!(t.state, TrustState::Untrusted(_)), "{t:?}");
    }
}

#[test]
fn every_judge_call_failing_is_the_one_failing_run() {
    struct Failing;
    impl DistillationJudge for Failing {
        fn judge(&self, _: &str, _: &str, _: &str) -> Result<AxisScores> {
            bail!("vendor down")
        }
    }
    impl ClaimJudge for Failing {
        fn judge_claims(&self, _: &str, _: &str, _: &[String]) -> Result<Vec<ClaimVerdict>> {
            bail!("vendor down")
        }
    }
    let fixtures = vec![fixture("t0")];
    let factory = |_: &EvalOpts| -> Result<(Box<dyn DistillationJudge>, Box<dyn ClaimJudge>)> {
        Ok((Box::new(Failing), Box::new(Failing)))
    };
    let env = env();
    let config = eval_config(3);
    let opts = EvalOpts::default();
    let run = InjectRun {
        opts: &opts,
        eval: &config,
        cache_path: &env.cache_path,
        now: now(),
    };
    let report = run_inject(&fixtures, &run, &env.scores, &env.trust, &factory).unwrap();
    assert!(report.all_judged_errored());
    let t = cell(&report, MODEL, CHECK_JUDGE_CLAIMS, PlantedClass::NumberSwap)
        .tally
        .unwrap();
    assert_eq!((t.n, t.errors), (0, 1));
}

#[test]
fn run_inject_refuses_an_empty_judge_model_list() {
    let env = env();
    // The default names two judges; only an explicit empty list reaches here.
    let config = EvalConfig {
        judge_models: Vec::new(),
        ..EvalConfig::default()
    };
    let opts = EvalOpts::default();
    let run = InjectRun {
        opts: &opts,
        eval: &config,
        cache_path: &env.cache_path,
        now: now(),
    };
    let factory = |_: &EvalOpts| -> Result<(Box<dyn DistillationJudge>, Box<dyn ClaimJudge>)> { bail!("unused") };
    let err = run_inject(&[fixture("t0")], &run, &env.scores, &env.trust, &factory).unwrap_err();
    assert!(err.to_string().contains("judge-models is empty"), "{err}");
}

#[test]
fn the_report_prints_pairwise_kappa_between_both_judges_and_never_a_dropped_tool() {
    const SECOND: &str = "claude-sonnet-5";
    let fixtures: Vec<Fixture> = (0..3).map(|i| fixture(&format!("t{i}"))).collect();
    let claims: HashSet<String> = fixtures
        .iter()
        .flat_map(|f| f.distilled.claims.iter().map(|c| c.text.trim().to_string()))
        .collect();
    // MODEL passes every original's claims; SECOND flags every claim.
    let factory = |o: &EvalOpts| -> Result<(Box<dyn DistillationJudge>, Box<dyn ClaimJudge>)> {
        let claim_judge: Box<dyn ClaimJudge> = if o.judge_model == MODEL {
            Box::new(KnownClaimsJudge { claims: claims.clone() })
        } else {
            Box::new(MockClaimJudge::new(ClaimVerdict::Unsupported))
        };
        Ok((
            Box::new(MockJudge::new(AxisScores {
                claim_coverage: 3,
                anchor_validity: 3,
                summary_faithfulness: 3,
            })),
            claim_judge,
        ))
    };
    let env = env();
    let config = EvalConfig {
        judge_models: vec![MODEL.to_string(), SECOND.to_string()],
        ..eval_config(3)
    };
    let opts = EvalOpts::default();
    let run = InjectRun {
        opts: &opts,
        eval: &config,
        cache_path: &env.cache_path,
        now: now(),
    };
    let report = run_inject(&fixtures, &run, &env.scores, &env.trust, &factory).unwrap();
    assert_eq!(report.agreement.notes, 3, "the originals only, never a mutant");
    let claims_pair = |report: &InjectReport| {
        report
            .agreement
            .trace_pairs
            .iter()
            .find(|p| p.a.check == CHECK_JUDGE_CLAIMS && p.b.check == CHECK_JUDGE_CLAIMS)
            .cloned()
            .expect("a judge-claims pair across the two models")
    };
    let pair = claims_pair(&report);
    assert_eq!(
        (pair.a.judge_model.as_str(), pair.b.judge_model.as_str()),
        (SECOND, MODEL)
    );
    // SECOND flags all 3 originals, MODEL none.
    assert_eq!((pair.table.n(), pair.table.a_only), (3, 3));
    let text = report.render();
    assert!(
        text.contains("pairwise judge agreement on the 3 unmutated originals"),
        "{text}"
    );
    let planted = text.find("check                judge").unwrap();
    let kappa = text.find("pairwise judge agreement").unwrap();
    let trust = text.find("trust (paired catch").unwrap();
    assert!(
        planted < kappa && kappa < trust,
        "kappa sits next to the planted-error table: {text}"
    );

    // A row the dropped MiniCheck tool left at an original's distill hash is
    // not a rater: it never joins the next report.
    let t0 = env.scores.rows_for("t0").unwrap();
    let original = t0
        .iter()
        .find(|r| r.run_kind == RUN_KIND_BASELINE && r.check_name == CHECK_JUDGE_CLAIMS)
        .unwrap();
    env.scores
        .put(&ScoreRow {
            judge_model: "roberta-large".to_string(),
            check_name: "minicheck".to_string(),
            rubric_version: crate::eval::cache::RUBRIC_VERSION.to_string(),
            metrics_json: serde_json::json!({
                "tool_version": "x", "metrics": {},
                "claims": { "1": { "supported": { "value": 0.9, "detail": { "label": 1 } } } }
            })
            .to_string(),
            ..original.clone()
        })
        .unwrap();
    let again = run_inject(&fixtures, &run, &env.scores, &env.trust, &factory).unwrap();
    let minicheck = crate::eval::agreement::Rater {
        check: "minicheck".to_string(),
        judge_model: "roberta-large".to_string(),
    };
    assert_eq!(again.agreement.raters.get(&minicheck), None);
    assert!(
        !again
            .agreement
            .claim_pairs
            .iter()
            .any(|p| p.a == minicheck || p.b == minicheck),
        "{:?}",
        again.agreement.claim_pairs
    );
}

#[test]
fn the_sample_keeps_only_traces_some_class_applies_to() {
    let tmp = tempfile::tempdir().unwrap();
    for (trace, claims) in [("t-two", 2), ("t-none", 0)] {
        let dir = tmp.path().join(trace);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("envelope.yml"), "kind: article-url\n").unwrap();
        let mut fx = fixture(trace);
        fx.distilled.claims.truncate(claims);
        std::fs::write(dir.join("distilled.yml"), serde_yaml::to_string(&fx.distilled).unwrap()).unwrap();
        std::fs::write(dir.join("transcript.md"), &fx.source).unwrap();
    }
    let picked = sample_fixtures(tmp.path(), 30).unwrap();
    assert_eq!(
        picked.iter().map(|f| f.slug.as_str()).collect::<Vec<_>>(),
        vec!["t-two"]
    );
}
