use super::*;
use crate::eval::scores::CHECK_QUOTE_GROUNDING;

const GEMINI: &str = "gemini-2.5-pro";
const CLAUDE: &str = "claude-sonnet-5";

fn row(trace: &str, model: &str, check: &str, rubric: &str, metrics: Value) -> ScoreRow {
    ScoreRow {
        trace: trace.to_string(),
        run_kind: RUN_KIND_BASELINE.to_string(),
        run_id: String::new(),
        kind: "article".to_string(),
        judge_model: model.to_string(),
        check_name: check.to_string(),
        rubric_version: rubric.to_string(),
        content_hash: format!("h-{trace}"),
        scored_at: "2026-10-17T00:00:00Z".to_string(),
        metrics_json: metrics.to_string(),
        error: None,
    }
}

fn claims(trace: &str, model: &str, verdicts: &[&str]) -> ScoreRow {
    row(
        trace,
        model,
        CHECK_JUDGE_CLAIMS,
        "v2",
        serde_json::json!({ "verdicts": verdicts }),
    )
}

fn table(both: usize, a_only: usize, b_only: usize, neither: usize) -> AgreementTable {
    AgreementTable {
        both,
        a_only,
        b_only,
        neither,
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

#[test]
fn kappa_matches_the_textbook_two_by_two() {
    // 20 both, 5 a-only, 10 b-only, 15 neither: p_o 0.70, p_a 0.50, p_b 0.60,
    // p_e 0.50, kappa 0.40.
    let t = table(20, 5, 10, 15);
    assert_eq!(t.n(), 50);
    assert!(close(t.observed().unwrap(), 0.70));
    assert!(close(t.kappa().unwrap(), 0.40), "{:?}", t.kappa());
}

#[test]
fn perfect_agreement_on_mixed_labels_is_kappa_one() {
    assert!(close(table(7, 0, 0, 3).kappa().unwrap(), 1.0));
}

#[test]
fn perfect_disagreement_on_balanced_labels_is_kappa_minus_one() {
    assert!(close(table(0, 5, 5, 0).kappa().unwrap(), -1.0));
}

#[test]
fn chance_level_agreement_is_kappa_zero() {
    // Independent raters at 0.5 each agree half the time.
    assert!(close(table(5, 5, 5, 5).kappa().unwrap(), 0.0));
}

#[test]
fn all_agree_on_one_label_is_undefined_not_perfect() {
    // Both raters flag every note: p_o = 1 and p_e = 1, so kappa is 0/0.
    let all_flagged = table(30, 0, 0, 0);
    assert_eq!(all_flagged.observed(), Some(1.0));
    assert_eq!(all_flagged.kappa(), None);
    // Both raters pass every note: same degenerate case.
    assert_eq!(table(0, 0, 0, 30).kappa(), None);
}

#[test]
fn one_rater_using_one_label_is_kappa_zero_not_undefined() {
    // A flags everything, B flags half: p_e = 0.5 and p_o = 0.5.
    let t = table(5, 5, 0, 0);
    assert!(close(t.kappa().unwrap(), 0.0), "{:?}", t.kappa());
}

#[test]
fn no_shared_item_is_undefined() {
    assert_eq!(AgreementTable::default().kappa(), None);
    assert_eq!(AgreementTable::default().observed(), None);
}

#[test]
fn trace_flags_follow_each_raters_rule() {
    let jd = |c, a, s| {
        row(
            "t",
            GEMINI,
            CHECK_JUDGE_DISTILLATION,
            "v2",
            serde_json::json!({ "claim_coverage": c, "anchor_validity": a, "summary_faithfulness": s }),
        )
    };
    assert_eq!(trace_flag(&jd(3, 3, 3)), Some(false));
    assert_eq!(trace_flag(&jd(3, 2, 3)), Some(true));

    assert_eq!(
        trace_flag(&claims("t", GEMINI, &["supported", "supported"])),
        Some(false)
    );
    assert_eq!(
        trace_flag(&claims("t", GEMINI, &["supported", "contradicted"])),
        Some(true)
    );

    let deepeval = |v: f64| {
        row(
            "t",
            "gpt-4o-mini",
            TOOL_DEEPEVAL,
            "v2",
            serde_json::json!({ "tool_version": "4.3.1", "metrics": { "summarization": { "value": v } } }),
        )
    };
    assert_eq!(trace_flag(&deepeval(0.5)), Some(false));
    assert_eq!(trace_flag(&deepeval(0.49)), Some(true));

    let minicheck = |labels: &[u64]| {
        let claims: serde_json::Map<String, Value> = labels
            .iter()
            .enumerate()
            .map(|(i, l)| {
                (
                    (i + 1).to_string(),
                    serde_json::json!({ "supported": { "value": 0.5, "detail": { "label": l } } }),
                )
            })
            .collect();
        row(
            "t",
            "roberta-large",
            TOOL_MINICHECK,
            "v2",
            serde_json::json!({ "tool_version": "x", "metrics": {}, "claims": claims }),
        )
    };
    assert_eq!(trace_flag(&minicheck(&[1, 1])), Some(false));
    assert_eq!(trace_flag(&minicheck(&[1, 0])), Some(true));
    assert_eq!(
        claim_support(&minicheck(&[1, 0])).unwrap(),
        BTreeMap::from([(1, true), (2, false)])
    );

    // A check with no flag rule is not a rater.
    assert_eq!(
        trace_flag(&row("t", "x", "some-new-tool", "v2", serde_json::json!({}))),
        None
    );
}

#[test]
fn only_current_error_free_baseline_judge_rows_are_raters() {
    let good = claims("t", GEMINI, &["supported"]);
    assert!(is_current_rater_row(&good));

    let mut errored = good.clone();
    errored.error = Some("503".into());
    assert!(!is_current_rater_row(&errored));

    let mut injected = good.clone();
    injected.run_kind = "inject".into();
    injected.run_id = "inject-x:number-swap".into();
    assert!(!is_current_rater_row(&injected));

    let mut stale_rubric = good.clone();
    stale_rubric.rubric_version = "v1".into();
    assert!(!is_current_rater_row(&stale_rubric));

    let deterministic = row(
        "t",
        NO_MODEL,
        CHECK_QUOTE_GROUNDING,
        "v1",
        serde_json::json!({"grounded": 1, "quoted": 1}),
    );
    assert!(!is_current_rater_row(&deterministic));
}

#[test]
fn agreement_pairs_two_vendors_on_shared_notes_at_both_levels() {
    // 4 notes. Gemini flags t1 and t2; Claude flags t1 and t3.
    let rows = vec![
        claims("t1", GEMINI, &["supported", "unsupported"]),
        claims("t1", CLAUDE, &["supported", "unsupported"]),
        claims("t2", GEMINI, &["contradicted"]),
        claims("t2", CLAUDE, &["supported"]),
        claims("t3", GEMINI, &["supported"]),
        claims("t3", CLAUDE, &["unsupported"]),
        claims("t4", GEMINI, &["supported"]),
        claims("t4", CLAUDE, &["supported"]),
        // An inject row and an error row never join.
        {
            let mut r = claims("t4", CLAUDE, &["unsupported"]);
            r.run_kind = "inject".into();
            r.run_id = "inject-x:number-swap".into();
            r
        },
        {
            let mut r = claims("t5", CLAUDE, &["unsupported"]);
            r.error = Some("503".into());
            r
        },
    ];
    let report = agreement(&rows);
    assert_eq!(report.notes, 4);
    assert_eq!(report.trace_pairs.len(), 1);
    let pair = &report.trace_pairs[0];
    // Raters sort by (check, model): claude before gemini.
    assert_eq!(pair.a.judge_model, CLAUDE);
    assert_eq!(pair.table, table(1, 1, 1, 1));
    assert!(close(pair.table.kappa().unwrap(), 0.0));

    // Claims: t1#1 ++, t1#2 --, t2#1 +- (claude supported, gemini not),
    // t3#1 -+, t4#1 ++.
    let claim_pair = &report.claim_pairs[0];
    assert_eq!(claim_pair.table, table(2, 1, 1, 1));
}

#[test]
fn a_rater_that_judged_an_older_distillation_is_not_paired() {
    let mut old = claims("t1", CLAUDE, &["unsupported"]);
    old.content_hash = "h-old".into();
    let rows = vec![claims("t1", GEMINI, &["supported"]), old];
    let report = agreement(&rows);
    assert_eq!(report.notes, 2, "two distinct notes: same trace, different hashes");
    assert!(report.trace_pairs.is_empty(), "{:?}", report.trace_pairs);
}

#[test]
fn render_prints_the_two_by_two_and_na_for_an_undefined_kappa() {
    let rows = vec![
        claims("t1", GEMINI, &["supported"]),
        claims("t1", CLAUDE, &["supported"]),
        claims("t2", GEMINI, &["supported"]),
        claims("t2", CLAUDE, &["supported"]),
    ];
    let rendered = agreement(&rows).render();
    assert!(
        rendered.contains("pairwise judge agreement on the 2 unmutated originals"),
        "{rendered}"
    );
    assert!(rendered.contains("judge-claims (claude-sonnet-5)"), "{rendered}");
    let line = rendered
        .lines()
        .find(|l| l.starts_with("judge-claims (claude-sonnet-5)") && l.contains("judge-claims (gemini-2.5-pro)"))
        .expect("a trace pair line");
    assert!(line.trim_end().ends_with("n/a"), "all-pass on both sides: {line}");
}
