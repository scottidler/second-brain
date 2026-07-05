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
    let report =
        unwrap_report(evaluate(&fixtures, &BTreeMap::new(), &cache, &judge, &EvalOpts::default()).expect("evaluate"));

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
    let r1 = unwrap_report(evaluate(&fixtures, &BTreeMap::new(), &cache, &judge1, &EvalOpts::default()).expect("run1"));
    assert_eq!(judge1.calls(), 2, "first run judges every fixture");
    assert_eq!(r1.new_judgments, 2);

    // Second run over the same cache with a judge that would score differently.
    let judge2 = MockJudge::new(scores(0, 0, 0));
    let r2 = unwrap_report(evaluate(&fixtures, &BTreeMap::new(), &cache, &judge2, &EvalOpts::default()).expect("run2"));
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
    let _ = evaluate(&fixtures, &BTreeMap::new(), &cache, &judge1, &EvalOpts::default()).expect("run1");

    let judge2 = MockJudge::new(scores(1, 1, 1));
    let opts = EvalOpts {
        rebuild_cache: true,
        ..EvalOpts::default()
    };
    let r2 = unwrap_report(evaluate(&fixtures, &BTreeMap::new(), &cache, &judge2, &opts).expect("run2"));
    assert_eq!(judge2.calls(), 2, "rebuild ignores the cache and re-judges");
    assert_eq!(r2.new_judgments, 2);
    assert!((r2.overall.composite - 1.0).abs() < 1e-9);
}

#[test]
fn truncation_flag_set_for_oversized_source() {
    let big = "x".repeat(JUDGE_SOURCE_MAX_CHARS + 10);
    let fixtures = vec![fixture("article", "big", &big, distilled("s", &[("c", None)], None))];
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let judge = MockJudge::new(scores(2, 2, 2));
    let report =
        unwrap_report(evaluate(&fixtures, &BTreeMap::new(), &cache, &judge, &EvalOpts::default()).expect("evaluate"));
    assert_eq!(report.truncated_judgments, 1);
}

#[test]
fn calibration_panel_present_when_labels_given() {
    let fixtures = two_kinds();
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let judge = MockJudge::new(scores(2, 2, 2));
    let mut calibration = BTreeMap::new();
    calibration.insert("video/vid-a".to_string(), scores(2, 2, 2)); // exact agreement
    let report =
        unwrap_report(evaluate(&fixtures, &calibration, &cache, &judge, &EvalOpts::default()).expect("evaluate"));
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
    match evaluate(&fixtures, &BTreeMap::new(), &cache, &judge, &opts).expect("evaluate") {
        EvalOutcome::CalibrationSheet(p) => assert_eq!(p, tmp.path()),
        EvalOutcome::Report(_) => panic!("expected a calibration sheet"),
    }
    let written = std::fs::read_to_string(tmp.path()).expect("read sheet");
    assert!(written.contains("fixture: video/vid-a"));
    assert!(written.contains("human-claim-coverage"));
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
