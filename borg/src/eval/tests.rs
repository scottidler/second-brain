use super::*;
use crate::eval::judge::AxisScores;
use std::collections::BTreeMap;
use vault::distilled::{Claim, Distilled, DistilledMeta, ValidationMeta};

fn scores(cc: u8, av: u8, sf: u8) -> AxisScores {
    AxisScores {
        claim_coverage: cc,
        anchor_validity: av,
        summary_faithfulness: sf,
    }
}

fn distilled(summary: &str, claims: &[(&str, Option<&str>)], fallback: Option<&str>) -> Distilled {
    Distilled {
        summary: summary.to_string(),
        claims: claims
            .iter()
            .map(|(t, a)| Claim {
                text: t.to_string(),
                anchor: a.map(|s| s.to_string()),
                ..Default::default()
            })
            .collect(),
        meta: DistilledMeta {
            validation: ValidationMeta {
                fallback_reason: fallback.map(|s| s.to_string()),
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    }
}

fn fixture(kind: &str, slug: &str, source: &str, distilled: Distilled) -> Fixture {
    Fixture {
        id: format!("{kind}/{slug}"),
        kind: kind.to_string(),
        slug: slug.to_string(),
        source: source.to_string(),
        distilled,
        capture_note: None,
    }
}

fn two_kinds() -> Vec<Fixture> {
    vec![
        fixture(
            "video",
            "vid-a",
            "long source about agents",
            distilled("agents summary", &[("agents beat autonomy", Some("00:14"))], None),
        ),
        fixture(
            "article",
            "art-a",
            "article source about rust",
            distilled("rust summary", &[("rust is fast", None)], Some("yaml-parse-error")),
        ),
    ]
}

fn unwrap_report(outcome: EvalOutcome) -> EvalReport {
    match outcome {
        EvalOutcome::Report(r) => *r,
        EvalOutcome::CalibrationSheet(_) => panic!("expected a report, got a calibration sheet"),
    }
}

#[test]
fn evaluate_produces_per_kind_and_overall_rows() {
    let fixtures = two_kinds();
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let judge = MockJudge::new(scores(1, 1, 1))
        .with("video", scores(3, 3, 2))
        .with("article", scores(2, 3, 3));
    let report = unwrap_report(
        evaluate(
            &fixtures,
            &BTreeMap::new(),
            &cache,
            &judge,
            &MockClaimJudge::new(ClaimVerdict::Supported),
            &EvalOpts::default(),
        )
        .expect("evaluate"),
    );

    assert_eq!(report.total_fixtures, 2);
    assert_eq!(report.total_judgments, 2);
    // sorted per-kind rows: article then video
    let kinds: Vec<&str> = report.kinds.iter().map(|k| k.kind.as_str()).collect();
    assert_eq!(kinds, vec!["article", "video"]);
    assert_eq!(report.overall.kind, OVERALL_LABEL);
    assert_eq!(report.overall.n, 2);

    // video composite = (3+3+2)/3, article = (2+3+3)/3; overall = mean of the two.
    let vid = report.kinds.iter().find(|k| k.kind == "video").expect("video row");
    assert!((vid.composite - (8.0 / 3.0)).abs() < 1e-9);
    // one fixture carried a fallback_reason.
    assert_eq!(report.fallback_fixtures, 1);
}

#[test]
fn rerun_is_cache_hit_stable_zero_new_judge_calls() {
    let fixtures = two_kinds();
    let cache = cache::JudgmentCache::open_memory().expect("cache");

    let judge1 = MockJudge::new(scores(2, 2, 2));
    let r1 = unwrap_report(
        evaluate(
            &fixtures,
            &BTreeMap::new(),
            &cache,
            &judge1,
            &MockClaimJudge::new(ClaimVerdict::Supported),
            &EvalOpts::default(),
        )
        .expect("run1"),
    );
    assert_eq!(judge1.calls(), 2, "first run judges every fixture");
    assert_eq!(r1.new_judgments, 2);

    // Second run over the same cache with a judge that would score differently.
    let judge2 = MockJudge::new(scores(0, 0, 0));
    let r2 = unwrap_report(
        evaluate(
            &fixtures,
            &BTreeMap::new(),
            &cache,
            &judge2,
            &MockClaimJudge::new(ClaimVerdict::Supported),
            &EvalOpts::default(),
        )
        .expect("run2"),
    );
    assert_eq!(judge2.calls(), 0, "cache-hit re-run makes zero judge calls");
    assert_eq!(r2.new_judgments, 0);
    // Cached scores win: composites match the first run, not the zero judge.
    assert!((r1.overall.composite - r2.overall.composite).abs() < 1e-9);
    assert!(r2.overall.composite > 0.0);
}

#[test]
fn rebuild_cache_forces_fresh_judgments() {
    let fixtures = two_kinds();
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let judge1 = MockJudge::new(scores(2, 2, 2));
    let _ = evaluate(
        &fixtures,
        &BTreeMap::new(),
        &cache,
        &judge1,
        &MockClaimJudge::new(ClaimVerdict::Supported),
        &EvalOpts::default(),
    )
    .expect("run1");

    let judge2 = MockJudge::new(scores(1, 1, 1));
    let opts = EvalOpts {
        rebuild_cache: true,
        ..EvalOpts::default()
    };
    let r2 = unwrap_report(
        evaluate(
            &fixtures,
            &BTreeMap::new(),
            &cache,
            &judge2,
            &MockClaimJudge::new(ClaimVerdict::Supported),
            &opts,
        )
        .expect("run2"),
    );
    assert_eq!(judge2.calls(), 2, "rebuild ignores the cache and re-judges");
    assert_eq!(r2.new_judgments, 2);
    assert!((r2.overall.composite - 1.0).abs() < 1e-9);
}

#[test]
fn truncation_flag_set_for_oversized_source() {
    let big = "x".repeat(JUDGE_INPUT_MAX_CHARS + 10);
    let fixtures = vec![fixture("article", "big", &big, distilled("s", &[("c", None)], None))];
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let judge = MockJudge::new(scores(2, 2, 2));
    let report = unwrap_report(
        evaluate(
            &fixtures,
            &BTreeMap::new(),
            &cache,
            &judge,
            &MockClaimJudge::new(ClaimVerdict::Supported),
            &EvalOpts::default(),
        )
        .expect("evaluate"),
    );
    assert_eq!(report.truncated_judgments, 1);
}

#[test]
fn calibration_panel_present_when_labels_given() {
    let fixtures = two_kinds();
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let judge = MockJudge::new(scores(2, 2, 2));
    let mut calibration = BTreeMap::new();
    calibration.insert("video/vid-a".to_string(), scores(2, 2, 2)); // exact agreement
    let report = unwrap_report(
        evaluate(
            &fixtures,
            &calibration,
            &cache,
            &judge,
            &MockClaimJudge::new(ClaimVerdict::Supported),
            &EvalOpts::default(),
        )
        .expect("evaluate"),
    );
    let c = report.calibration.expect("calibration present");
    assert_eq!(c.pairs, 3); // one fixture x three axes
    assert!((c.exact_pct - 1.0).abs() < 1e-9);
    assert!(c.trustworthy);
}

#[test]
fn emit_calibration_writes_sheet_and_short_circuits() {
    let fixtures = two_kinds();
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let judge = MockJudge::new(scores(2, 3, 1));
    let tmp = tempfile::NamedTempFile::new().expect("tmp");
    let opts = EvalOpts {
        emit_calibration: Some(tmp.path().to_path_buf()),
        ..EvalOpts::default()
    };
    match evaluate(
        &fixtures,
        &BTreeMap::new(),
        &cache,
        &judge,
        &MockClaimJudge::new(ClaimVerdict::Supported),
        &opts,
    )
    .expect("evaluate")
    {
        EvalOutcome::CalibrationSheet(p) => assert_eq!(p, tmp.path()),
        EvalOutcome::Report(_) => panic!("expected a calibration sheet"),
    }
    let written = std::fs::read_to_string(tmp.path()).expect("read sheet");
    assert!(written.contains("fixture: video/vid-a"));
    assert!(written.contains("human-claim-coverage"));
}

// --- deterministic metrics wiring -------------------------------

/// Design-doc acceptance criterion #4 against the real committed fixture set:
/// `sb borg eval` (here, its `evaluate()` core) reports listicle-survival =
/// 1.0 on the April fixture, and enforces note-size across every fixture. The
/// per-fixture is-listicle gate (an expected `declared-count`) excludes every
/// non-listicle fixture from the aggregate - it does not fold them in as 0.0.
#[test]
fn evaluate_gates_listicle_to_declared_count_fixtures_on_real_fixture_set() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../config/eval/distill-fixtures");
    let fixtures = load(&dir).expect("load committed fixtures");
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let judge = MockJudge::new(scores(2, 2, 2));
    let report = unwrap_report(
        evaluate(
            &fixtures,
            &BTreeMap::new(),
            &cache,
            &judge,
            &MockClaimJudge::new(ClaimVerdict::Supported),
            &EvalOpts::default(),
        )
        .expect("evaluate"),
    );

    // Exactly one committed fixture declares a declared-count today: the
    // April-shape fixture. It is the only row in the listicle aggregate.
    assert_eq!(
        report.listicle.len(),
        1,
        "only the April fixture declares a declared-count"
    );
    assert_eq!(
        report.listicle[0].fixture,
        "video/top-10-claude-code-skills-plugins-clis-april-2026"
    );
    assert_eq!(report.listicle[0].score, 1.0);
    assert_eq!(report.listicle_aggregate, Some(1.0));

    // A non-listicle video fixture (no enumeration at all) must be EXCLUDED
    // from the aggregate, not present scored at 0.0.
    assert!(
        !report
            .listicle
            .iter()
            .any(|m| m.fixture == "video/there-are-only-5-safe-places-to-build-in-ai-right-now-are-yo"),
        "a fixture with no declared-count is N/A, not a 0.0 row"
    );

    // note-size is enforced across every fixture in the set (all committed
    // fixtures - full kind span - stay under the ceiling today).
    assert_eq!(report.note_size.len(), fixtures.len());
    assert!(
        report.note_size.iter().all(|m| m.within_ceiling),
        "every committed fixture must render under the ceiling"
    );
}

/// Break-the-code guarantee: a listicle fixture whose distilled artifact LOST
/// its enumeration items (declared 10, delivered 0 - the exact regression
/// class this design exists to catch) scores 0.0 and drags the aggregate down
/// alongside a genuinely-full listicle fixture, proving the metric bites.
#[test]
fn evaluate_scores_zero_and_drags_aggregate_when_enumeration_items_are_lost() {
    use vault::distilled::Enumeration;

    let mut intact = distilled("a top-10 video, fully survived", &[("item one", None)], None);
    intact.enumeration = Some(Enumeration {
        lead_in: None,
        declared_count: Some(10),
        items: (1..=10)
            .map(|n| vault::distilled::EnumeratedItem {
                name: format!("item {n}"),
                text: "text".to_string(),
                anchor: None,
            })
            .collect(),
    });

    let mut regressed = distilled("a top-10 video that lost its list", &[("item one", None)], None);
    regressed.enumeration = Some(Enumeration {
        lead_in: None,
        declared_count: Some(10),
        items: Vec::new(), // the enumeration survived as a shell but every item vanished
    });

    let fixtures = vec![
        fixture("video", "intact-listicle", "source a", intact),
        fixture("video", "regressed-listicle", "source b", regressed),
    ];
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let judge = MockJudge::new(scores(2, 2, 2));
    let report = unwrap_report(
        evaluate(
            &fixtures,
            &BTreeMap::new(),
            &cache,
            &judge,
            &MockClaimJudge::new(ClaimVerdict::Supported),
            &EvalOpts::default(),
        )
        .expect("evaluate"),
    );

    assert_eq!(report.listicle.len(), 2, "both fixtures declare a declared-count");
    let regressed_score = report
        .listicle
        .iter()
        .find(|m| m.fixture == "video/regressed-listicle")
        .expect("regressed fixture present")
        .score;
    assert_eq!(
        regressed_score, 0.0,
        "an emptied enumeration must score zero, not vacuous credit"
    );
    // Aggregate is the mean of 1.0 (intact) and 0.0 (regressed) - the drop is visible.
    assert_eq!(report.listicle_aggregate, Some(0.5));
}

/// Success-criterion guard: the committed fixture set has >= 20 fixtures
/// spanning all content kinds. Located via `CARGO_MANIFEST_DIR` so the test is
/// independent of the process working directory.
#[test]
fn committed_fixtures_meet_minimum_and_span_kinds() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../config/eval/distill-fixtures");
    let fixtures = load(&dir).expect("load committed fixtures");
    assert!(
        fixtures.len() >= 20,
        "expected >= 20 committed fixtures, found {}",
        fixtures.len()
    );
    let kinds: std::collections::BTreeSet<&str> = fixtures.iter().map(|f| f.kind.as_str()).collect();
    for expected in ["article", "video", "thread", "repo", "image", "voicenote", "idea"] {
        assert!(kinds.contains(expected), "missing fixture kind: {expected}");
    }
}

#[test]
fn claim_verdicts_tally_per_kind_and_flag_non_supported() {
    let fixtures = two_kinds();
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let judge = MockJudge::new(scores(2, 2, 2));
    let claims = MockClaimJudge::new(ClaimVerdict::Supported).with("rust is fast", ClaimVerdict::Contradicted);
    let r = unwrap_report(
        evaluate(
            &fixtures,
            &BTreeMap::new(),
            &cache,
            &judge,
            &claims,
            &EvalOpts::default(),
        )
        .expect("evaluate"),
    );
    assert_eq!(r.claim_support_overall.supported, 1);
    assert_eq!(r.claim_support_overall.contradicted, 1);
    assert_eq!(r.flagged_claims.len(), 1);
    assert_eq!(r.flagged_claims[0].fixture, "article/art-a");
    assert_eq!(r.new_claim_judgments, 2);
}

#[test]
fn claim_verdicts_are_cache_hit_stable() {
    let fixtures = two_kinds();
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let judge = MockJudge::new(scores(2, 2, 2));
    let first = MockClaimJudge::new(ClaimVerdict::Supported);
    let _ = evaluate(
        &fixtures,
        &BTreeMap::new(),
        &cache,
        &judge,
        &first,
        &EvalOpts::default(),
    )
    .expect("run1");
    let second = MockClaimJudge::new(ClaimVerdict::Contradicted);
    let r = unwrap_report(
        evaluate(
            &fixtures,
            &BTreeMap::new(),
            &cache,
            &judge,
            &second,
            &EvalOpts::default(),
        )
        .expect("run2"),
    );
    assert_eq!(second.calls(), 0);
    assert_eq!(r.new_claim_judgments, 0);
    assert_eq!(r.claim_support_overall.supported, 2);
}

#[test]
fn no_judge_makes_zero_judge_calls_and_still_grounds_quotes() {
    let mut fixtures = two_kinds();
    fixtures[0].source = "Agents beat autonomy, said nobody.".to_string();
    fixtures[0].distilled.claims[0].quote = Some("agents beat autonomy".to_string());
    fixtures[1].distilled.claims[0].quote = Some("a quote that is not there".to_string());
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let judge = MockJudge::new(scores(2, 2, 2));
    let claims = MockClaimJudge::new(ClaimVerdict::Supported);
    let opts = EvalOpts {
        no_judge: true,
        ..EvalOpts::default()
    };
    let r = unwrap_report(evaluate(&fixtures, &BTreeMap::new(), &cache, &judge, &claims, &opts).expect("evaluate"));
    assert_eq!(judge.calls(), 0);
    assert_eq!(claims.calls(), 0);
    assert!(!r.judged);
    let g: Vec<(usize, usize)> = r.grounding.iter().map(|g| (g.grounded, g.quoted)).collect();
    assert_eq!(g, vec![(1, 1), (0, 1)]);
}

#[test]
fn no_judge_refuses_emit_calibration() {
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let opts = EvalOpts {
        no_judge: true,
        emit_calibration: Some(std::path::PathBuf::from("/nonexistent/sheet.yml")),
        ..EvalOpts::default()
    };
    let judge = MockJudge::new(scores(2, 2, 2));
    let claims = MockClaimJudge::new(ClaimVerdict::Supported);
    assert!(evaluate(&two_kinds(), &BTreeMap::new(), &cache, &judge, &claims, &opts).is_err());
}

fn tools_grounding_line(fixtures: &[Fixture]) -> String {
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let opts = EvalOpts {
        no_judge: true,
        ..EvalOpts::default()
    };
    let report = unwrap_report(
        evaluate(
            fixtures,
            &BTreeMap::new(),
            &cache,
            &MockJudge::new(scores(3, 3, 3)),
            &MockClaimJudge::new(ClaimVerdict::Supported),
            &opts,
        )
        .expect("evaluate"),
    );
    report
        .render()
        .lines()
        .find(|l| l.trim_start().starts_with("tools-grounding:"))
        .expect("the report prints a tools-grounding line")
        .to_string()
}

#[test]
fn removing_a_fixtures_tools_changes_the_tools_grounding_line() {
    let mut with_tools = distilled("s", &[("c", None)], None);
    with_tools.tools = vec![vault::distilled::Tool {
        name: "ripgrep".to_string(),
        url: None,
        command: Some("cargo install ripgrep".to_string()),
    }];
    let source = "install ripgrep with cargo install ripgrep";
    let before = tools_grounding_line(&[fixture("repo", "r", source, with_tools.clone())]);
    assert_eq!(
        before,
        "  tools-grounding: names 1/1 (100.0%), commands 1/1 (100.0%)  (1 fixture)"
    );

    let mut without_tools = with_tools;
    without_tools.tools.clear();
    let after = tools_grounding_line(&[fixture("repo", "r", source, without_tools)]);
    assert_eq!(after, "  tools-grounding: N/A (no note renders a tool)");
    assert_ne!(before, after);
}

#[test]
fn committed_fixtures_report_prints_a_tools_grounding_line() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../config/eval/distill-fixtures");
    let fixtures = load(&dir).expect("load committed fixtures");
    let line = tools_grounding_line(&fixtures);
    assert!(line.contains("tools-grounding:"), "{line}");
}

/// A distillation judge that records the `(source, note)` it was handed.
#[derive(Default)]
struct RecordingJudge {
    seen: std::sync::Mutex<Vec<(String, String)>>,
}

impl DistillationJudge for RecordingJudge {
    fn judge(&self, _kind: &str, source: &str, note: &str) -> Result<AxisScores> {
        self.seen
            .lock()
            .expect("lock")
            .push((source.to_string(), note.to_string()));
        Ok(scores(3, 3, 3))
    }
}

/// A 30,000-char source still leaves the full note text
/// in the judge input, after fabric's head truncation at the input budget.
#[test]
fn a_30k_source_leaves_the_full_note_in_the_judge_input() {
    let source = "word ".repeat(6_000);
    assert_eq!(source.chars().count(), 30_000);
    let mut d = distilled("a long summary of the source", &[("first claim", Some("00:01"))], None);
    d.claims[0].quote = Some("word word".to_string());
    d.claims[0].who = Some("the author".to_string());
    let fixtures = vec![fixture("video", "long", &source, d.clone())];
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let judge = RecordingJudge::default();
    let report = unwrap_report(
        evaluate(
            &fixtures,
            &BTreeMap::new(),
            &cache,
            &judge,
            &MockClaimJudge::new(ClaimVerdict::Supported),
            &EvalOpts::default(),
        )
        .expect("evaluate"),
    );

    let (seen_source, seen_note) = judge.seen.lock().expect("lock")[0].clone();
    let note = judge_note_text(&d);
    assert_eq!(seen_note, note);
    // What fabric actually receives: the head of the input at the budget.
    let input = judge::distillation_input("video", &seen_note, &seen_source);
    let sent = vault::text::truncate(&input, JUDGE_INPUT_MAX_CHARS);
    assert!(sent.contains(&note), "the full note must survive truncation");
    assert!(sent.contains("quote: \"word word\""));
    assert!(seen_source.chars().count() < source.chars().count());
    assert_eq!(report.truncated_judgments, 1);
    assert!(report.traces[0].source_truncated);
}

/// A distillation judge that fails on any note containing `needle`.
struct FailingJudge {
    needle: &'static str,
}

impl DistillationJudge for FailingJudge {
    fn judge(&self, _kind: &str, _source: &str, note: &str) -> Result<AxisScores> {
        if note.contains(self.needle) {
            eyre::bail!("fabric -p judge-distillation failed: vendor 503");
        }
        Ok(scores(2, 2, 2))
    }
}

#[test]
fn a_judge_error_is_recorded_on_its_trace_and_the_run_continues() {
    let fixtures = vec![
        fixture("article", "bad", "src", distilled("bad summary", &[("c1", None)], None)),
        fixture(
            "article",
            "good",
            "src",
            distilled("good summary", &[("c2", None)], None),
        ),
    ];
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let report = unwrap_report(
        evaluate(
            &fixtures,
            &BTreeMap::new(),
            &cache,
            &FailingJudge { needle: "bad summary" },
            &MockClaimJudge::new(ClaimVerdict::Supported),
            &EvalOpts::default(),
        )
        .expect("one judge error must not abort the run"),
    );
    let bad = report.traces.iter().find(|t| t.trace == "bad").expect("bad trace");
    assert_eq!(bad.errors.len(), 1);
    assert_eq!(bad.errors[0].check, CHECK_JUDGE_DISTILLATION);
    assert!(bad.errors[0].error.contains("vendor 503"));
    assert_eq!(bad.metrics.judge_distillation, None);
    // Its claims still got judged.
    assert!(bad.metrics.judge_claims.verdicts().is_some());
    let good = report.traces.iter().find(|t| t.trace == "good").expect("good trace");
    assert!(good.errors.is_empty());
    assert_eq!(good.metrics.judge_distillation, Some(scores(2, 2, 2)));
    assert_eq!(report.total_judgments, 1);
    assert_eq!(report.errored_traces(), 1);
    assert!(!report.all_judged_errored());
    assert!(report.render().contains("judge errors: 1 of 2 traces"));
}

#[test]
fn every_judged_trace_erroring_is_the_one_failing_run() {
    let fixtures = two_kinds();
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let report = unwrap_report(
        evaluate(
            &fixtures,
            &BTreeMap::new(),
            &cache,
            &FailingJudge { needle: "summary" },
            &MockClaimJudge::new(ClaimVerdict::Supported),
            &EvalOpts::default(),
        )
        .expect("evaluate"),
    );
    assert_eq!(report.errored_traces(), 2);
    assert!(report.all_judged_errored());
}

#[test]
fn a_no_judge_run_never_counts_as_all_errored() {
    let opts = EvalOpts {
        no_judge: true,
        ..Default::default()
    };
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let report = unwrap_report(
        evaluate(
            &two_kinds(),
            &BTreeMap::new(),
            &cache,
            &FailingJudge { needle: "summary" },
            &MockClaimJudge::new(ClaimVerdict::Supported),
            &opts,
        )
        .expect("evaluate"),
    );
    assert_eq!(report.errored_traces(), 0);
    assert!(!report.all_judged_errored());
}

/// A claim judge that returns one verdict too few.
struct ShortClaimJudge;

impl ClaimJudge for ShortClaimJudge {
    fn judge_claims(&self, _kind: &str, _source: &str, claims: &[String]) -> Result<Vec<ClaimVerdict>> {
        Ok(vec![ClaimVerdict::Supported; claims.len().saturating_sub(1)])
    }
}

#[test]
fn a_short_claim_verdict_list_is_an_error_row_not_an_abort() {
    let fixtures = vec![fixture(
        "article",
        "two-claims",
        "src",
        distilled("s", &[("c1", None), ("c2", None)], None),
    )];
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let report = unwrap_report(
        evaluate(
            &fixtures,
            &BTreeMap::new(),
            &cache,
            &MockJudge::new(scores(3, 3, 3)),
            &ShortClaimJudge,
            &EvalOpts::default(),
        )
        .expect("evaluate"),
    );
    let t = &report.traces[0];
    assert_eq!(t.errors.len(), 1);
    assert_eq!(t.errors[0].check, CHECK_JUDGE_CLAIMS);
    assert!(
        t.errors[0].error.contains("1 verdicts for 2 claims"),
        "{}",
        t.errors[0].error
    );
    assert_eq!(t.metrics.judge_claims, ClaimsOutcome::NotRun);
    assert!(t.metrics.judge_distillation.is_some());
}

#[test]
fn a_trace_record_serializes_to_the_jsonl_contract() {
    let mut d = distilled("s", &[("c1", Some("00:01"))], None);
    d.claims[0].quote = Some("verbatim".to_string());
    let fixtures = vec![fixture("video", "cl-abc", "a source with verbatim words", d)];
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let report = unwrap_report(
        evaluate(
            &fixtures,
            &BTreeMap::new(),
            &cache,
            &MockJudge::new(scores(3, 2, 1)),
            &MockClaimJudge::new(ClaimVerdict::Contradicted),
            &EvalOpts::default(),
        )
        .expect("evaluate"),
    );
    let json: serde_json::Value = serde_json::to_value(&report.traces[0]).expect("serialize");
    for key in [
        "trace",
        "run_kind",
        "run_id",
        "rubric_version",
        "kind",
        "content_hash",
        "source",
        "source_truncated",
        "note_text",
        "claims",
        "metrics",
        "errors",
    ] {
        assert!(json.get(key).is_some(), "missing {key}: {json}");
    }
    assert_eq!(json["trace"], "cl-abc");
    assert_eq!(json["run_kind"], "baseline");
    assert_eq!(json["run_id"], "");
    assert_eq!(json["rubric_version"], cache::RUBRIC_VERSION);
    assert_eq!(json["claims"][0]["id"], 1);
    assert_eq!(json["claims"][0]["quote"], "verbatim");
    assert!(json["claims"][0].get("who").is_none(), "absent who is omitted: {json}");
    assert_eq!(json["metrics"]["judge_distillation"]["claim_coverage"], 3);
    assert_eq!(json["metrics"]["judge_claims"][0], "contradicted");
    assert_eq!(json["metrics"]["quote_grounding"]["grounded"], 1);
    // The distill hash covers exactly the judged source and note.
    let t = &report.traces[0];
    let expected = cache::stable_hash(&format!("{}\n{}\n{}", t.kind, t.source, t.note_text));
    assert_eq!(t.content_hash, expected);
}

#[test]
fn a_no_judge_run_still_emits_one_record_per_fixture() {
    let opts = EvalOpts {
        no_judge: true,
        ..Default::default()
    };
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let fixtures = two_kinds();
    let report = unwrap_report(
        evaluate(
            &fixtures,
            &BTreeMap::new(),
            &cache,
            &MockJudge::new(scores(3, 3, 3)),
            &MockClaimJudge::new(ClaimVerdict::Supported),
            &opts,
        )
        .expect("evaluate"),
    );
    assert_eq!(report.traces.len(), fixtures.len());
    assert!(report.traces.iter().all(|t| t.metrics.judge_distillation.is_none()));
    assert!(report.traces.iter().all(|t| !t.content_hash.is_empty()));
}

/// The judgeability gate: a source over `eval.claims-source-max-chars` makes
/// no claims call and is written as a not-judgeable row, which covers the
/// trace until the cap is raised past its length.
#[test]
fn judge_claims_over_cap_is_not_judgeable() {
    const MODEL: &str = "gemini-2.5-pro";
    let source = "x".repeat(500_000);
    let fixtures = vec![fixture(
        "article",
        "huge",
        &source,
        distilled("s", &[("c1", None)], None),
    )];
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let claim_judge = MockClaimJudge::new(ClaimVerdict::Supported);
    let opts = EvalOpts {
        judge_model: MODEL.to_string(),
        ..EvalOpts::default()
    };
    assert_eq!(
        opts.claims_source_max_chars,
        crate::config::DEFAULT_CLAIMS_SOURCE_MAX_CHARS
    );
    let report = unwrap_report(
        evaluate(
            &fixtures,
            &BTreeMap::new(),
            &cache,
            &MockJudge::new(scores(3, 3, 3)),
            &claim_judge,
            &opts,
        )
        .expect("evaluate"),
    );
    assert_eq!(
        claim_judge.calls(),
        0,
        "an over-cap source never reaches the claims judge"
    );
    let t = &report.traces[0];
    assert_eq!(
        t.metrics.judge_claims,
        ClaimsOutcome::NotJudgeable {
            source_chars: 500_000,
            cap: 400_000
        }
    );
    assert!(t.errors.is_empty(), "not judgeable is not an error: {:?}", t.errors);
    assert_eq!(report.not_judgeable_claims, 1);
    assert!(report.render().contains("claims not judgeable: 1 traces"));

    let store = scores::ScoreStore::open_memory().expect("store");
    let rows = scores::rows_from_record(t, true, "2026-10-18T00:00:00Z");
    let claims_row = rows
        .iter()
        .find(|r| r.check_name == CHECK_JUDGE_CLAIMS)
        .expect("a judge-claims row");
    assert_eq!(claims_row.error, None);
    let metrics: serde_json::Value = serde_json::from_str(&claims_row.metrics_json).expect("json");
    assert_eq!(
        metrics,
        serde_json::json!({ "applicable": false, "reason": "source-over-cap", "source_chars": 500000, "cap": 400000 })
    );
    for row in &rows {
        store.put(row).expect("put");
    }
    let required = scores::required_checks(&[MODEL.to_string()]);
    assert!(
        store
            .is_covered(&t.trace, &t.content_hash, &required, 400_000)
            .expect("covered"),
        "a not-judgeable row covers its check under the cap that made it"
    );
    assert!(
        !store
            .is_covered(&t.trace, &t.content_hash, &required, 600_000)
            .expect("covered"),
        "raising the cap past the source re-opens the trace"
    );
}

/// Under the cap, the claims judge reads the whole source: never the
/// distillation judge's budgeted one, never trimmed.
#[test]
fn judge_claims_reads_the_whole_source_under_the_cap() {
    let source = "y".repeat(300_000);
    let fixtures = vec![fixture(
        "article",
        "long",
        &source,
        distilled("s", &[("c1", None)], None),
    )];
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let claim_judge = MockClaimJudge::new(ClaimVerdict::Supported);
    let report = unwrap_report(
        evaluate(
            &fixtures,
            &BTreeMap::new(),
            &cache,
            &MockJudge::new(scores(3, 3, 3)),
            &claim_judge,
            &EvalOpts::default(),
        )
        .expect("evaluate"),
    );
    assert_eq!(claim_judge.source_chars(), vec![300_000]);
    assert_eq!(
        report.traces[0].metrics.judge_claims,
        ClaimsOutcome::Judged(vec![ClaimVerdict::Supported])
    );
    // The distillation judge still sees its 24k budget.
    assert!(report.traces[0].source_truncated);
}
