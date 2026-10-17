use super::*;
use crate::eval::judge::{AxisScores, ClaimVerdict};
use crate::eval::report::{TraceError, TraceMetrics};

fn row(trace: &str, run_kind: &str, run_id: &str, model: &str, check: &str, hash: &str) -> ScoreRow {
    ScoreRow {
        trace: trace.to_string(),
        run_kind: run_kind.to_string(),
        run_id: run_id.to_string(),
        kind: "article".to_string(),
        judge_model: model.to_string(),
        check_name: check.to_string(),
        rubric_version: RUBRIC_VERSION.to_string(),
        content_hash: hash.to_string(),
        scored_at: "2026-10-17T00:00:00Z".to_string(),
        metrics_json: "{}".to_string(),
        error: None,
    }
}

fn required_one_judge() -> Vec<RequiredCheck> {
    required_checks(&["gemini-2.5-pro".to_string()])
}

/// A baseline row for every required check at `hash`.
fn cover(store: &ScoreStore, trace: &str, hash: &str, required: &[RequiredCheck]) {
    for check in required {
        let mut r = row(
            trace,
            RUN_KIND_BASELINE,
            "",
            &check.judge_model,
            &check.check_name,
            hash,
        );
        r.rubric_version = check.rubric_version.clone();
        store.put(&r).unwrap();
    }
}

#[test]
fn baseline_and_candidate_rows_for_the_same_trace_and_judge_both_persist() {
    let store = ScoreStore::open_memory().unwrap();
    let judge = "gemini-2.5-pro";
    store
        .put(&row("tr-1", "baseline", "", judge, CHECK_JUDGE_DISTILLATION, "h1"))
        .unwrap();
    store
        .put(&row(
            "tr-1",
            "candidate",
            "distill-video-aaaa",
            judge,
            CHECK_JUDGE_DISTILLATION,
            "h1",
        ))
        .unwrap();
    let kinds: Vec<String> = store
        .rows_for("tr-1")
        .unwrap()
        .into_iter()
        .map(|r| r.run_kind)
        .collect();
    assert_eq!(kinds, vec!["baseline", "candidate"]);
}

#[test]
fn two_candidates_with_identical_output_do_not_share_a_row() {
    let store = ScoreStore::open_memory().unwrap();
    // Same trace, judge, check and content hash (the outputs are identical):
    // only run_id tells the candidates apart.
    for run_id in ["distill-video-aaaa", "distill-video-bbbb"] {
        store
            .put(&row(
                "tr-1",
                "candidate",
                run_id,
                "gemini-2.5-pro",
                CHECK_JUDGE_DISTILLATION,
                "same-hash",
            ))
            .unwrap();
    }
    // A candidate whose output equals the baseline's still gets its own row.
    store
        .put(&row(
            "tr-1",
            "baseline",
            "",
            "gemini-2.5-pro",
            CHECK_JUDGE_DISTILLATION,
            "same-hash",
        ))
        .unwrap();
    assert_eq!(store.rows_for("tr-1").unwrap().len(), 3);
}

#[test]
fn an_error_row_never_replaces_a_success_but_a_success_replaces_an_error() {
    let store = ScoreStore::open_memory().unwrap();
    let mut ok = row("tr-1", "baseline", "", "m", CHECK_JUDGE_CLAIMS, "h");
    ok.metrics_json = r#"{"verdicts":["supported"]}"#.to_string();
    store.put(&ok).unwrap();
    let mut failed = ok.clone();
    failed.error = Some("429".to_string());
    failed.metrics_json = "{}".to_string();
    store.put(&failed).unwrap();
    let rows = store.rows_for("tr-1").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].error, None, "the success survives a later failed re-score");
    assert_eq!(rows[0].metrics_json, ok.metrics_json);

    let store = ScoreStore::open_memory().unwrap();
    store.put(&failed).unwrap();
    store.put(&ok).unwrap();
    let rows = store.rows_for("tr-1").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].error, None, "a retry that succeeds replaces the error row");
}

#[test]
fn a_trace_with_every_required_check_current_and_clean_is_covered() {
    let store = ScoreStore::open_memory().unwrap();
    let required = required_one_judge();
    cover(&store, "tr-1", "h1", &required);
    assert!(store.is_covered("tr-1", "h1", &required).unwrap());
}

#[test]
fn a_trace_whose_only_current_row_for_a_required_check_is_an_error_is_not_covered() {
    let required = required_one_judge();
    let claims = required.iter().find(|c| c.check_name == CHECK_JUDGE_CLAIMS).unwrap();
    let store2 = ScoreStore::open_memory().unwrap();
    for check in required.iter().filter(|c| *c != claims) {
        let mut r = row("tr-1", "baseline", "", &check.judge_model, &check.check_name, "h1");
        r.rubric_version = check.rubric_version.clone();
        store2.put(&r).unwrap();
    }
    let mut err = row("tr-1", "baseline", "", &claims.judge_model, CHECK_JUDGE_CLAIMS, "h1");
    err.rubric_version = claims.rubric_version.clone();
    err.error = Some("timeout".to_string());
    store2.put(&err).unwrap();
    assert!(!store2.is_covered("tr-1", "h1", &required).unwrap());
}

#[test]
fn a_trace_scored_only_by_a_judge_outside_the_required_set_is_not_covered() {
    let store = ScoreStore::open_memory().unwrap();
    let other = required_checks(&["claude-sonnet-5".to_string()]);
    cover(&store, "tr-1", "h1", &other);
    assert!(!store.is_covered("tr-1", "h1", &required_one_judge()).unwrap());
}

#[test]
fn a_row_for_an_older_content_hash_does_not_cover_the_current_one() {
    let store = ScoreStore::open_memory().unwrap();
    let required = required_one_judge();
    cover(&store, "tr-1", "old-hash", &required);
    assert!(!store.is_covered("tr-1", "new-hash", &required).unwrap());
}

#[test]
fn a_row_for_an_older_rubric_version_does_not_cover() {
    let store = ScoreStore::open_memory().unwrap();
    let required = required_one_judge();
    for check in &required {
        let mut r = row("tr-1", "baseline", "", &check.judge_model, &check.check_name, "h1");
        r.rubric_version = "v0".to_string();
        store.put(&r).unwrap();
    }
    assert!(!store.is_covered("tr-1", "h1", &required).unwrap());
}

#[test]
fn candidate_and_inject_rows_never_cover_a_trace() {
    let store = ScoreStore::open_memory().unwrap();
    let required = required_one_judge();
    for check in &required {
        for (run_kind, run_id) in [("candidate", "p-aaaa"), ("inject", "run1-number-swap")] {
            let mut r = row("tr-1", run_kind, run_id, &check.judge_model, &check.check_name, "h1");
            r.rubric_version = check.rubric_version.clone();
            store.put(&r).unwrap();
        }
    }
    assert!(!store.is_covered("tr-1", "h1", &required).unwrap());
}

#[test]
fn an_empty_required_set_covers_nothing() {
    let store = ScoreStore::open_memory().unwrap();
    assert!(!store.is_covered("tr-1", "h1", &[]).unwrap());
}

#[test]
fn required_checks_are_the_deterministic_set_plus_both_judge_checks_per_model() {
    let required = required_checks(&["a".to_string(), "b".to_string()]);
    assert_eq!(required.len(), DETERMINISTIC_CHECKS.len() + 4);
    assert!(required.iter().any(|c| c.judge_model == "b"
        && c.check_name == CHECK_JUDGE_CLAIMS
        && c.rubric_version == CLAIMS_RUBRIC_VERSION));
    assert_eq!(required_checks(&[]).len(), DETERMINISTIC_CHECKS.len());
}

fn record(judge_model: &str) -> TraceRecord {
    TraceRecord {
        trace: "tr-1".to_string(),
        fixture: "article/tr-1".to_string(),
        run_kind: RUN_KIND_BASELINE.to_string(),
        run_id: String::new(),
        rubric_version: RUBRIC_VERSION.to_string(),
        judge_model: judge_model.to_string(),
        kind: "article".to_string(),
        content_hash: "h1".to_string(),
        source_truncated: false,
        claims: Vec::new(),
        note_text: "note".to_string(),
        source: "source".to_string(),
        metrics: TraceMetrics::default(),
        errors: Vec::new(),
    }
}

#[test]
fn deterministic_rows_are_written_for_every_check_even_when_not_applicable() {
    let rows = rows_from_record(&record("m"), false, "2026-10-17T00:00:00Z");
    let names: Vec<&str> = rows.iter().map(|r| r.check_name.as_str()).collect();
    assert_eq!(names, DETERMINISTIC_CHECKS);
    assert!(rows.iter().all(|r| r.judge_model.is_empty() && r.error.is_none()));
    let quote = rows.iter().find(|r| r.check_name == CHECK_QUOTE_GROUNDING).unwrap();
    assert_eq!(quote.metrics_json, r#"{"applicable":false}"#);
}

#[test]
fn judge_rows_carry_verdicts_and_a_failed_call_becomes_an_error_row() {
    let mut ok = record("gemini-2.5-pro");
    ok.metrics.judge_distillation = Some(AxisScores {
        claim_coverage: 3,
        anchor_validity: 2,
        summary_faithfulness: 3,
    });
    ok.metrics.judge_claims = Some(vec![ClaimVerdict::Supported, ClaimVerdict::Unsupported]);
    let rows = rows_from_record(&ok, true, "t");
    let claims = rows.iter().find(|r| r.check_name == CHECK_JUDGE_CLAIMS).unwrap();
    assert_eq!(claims.judge_model, "gemini-2.5-pro");
    assert_eq!(claims.metrics_json, r#"{"verdicts":["supported","unsupported"]}"#);
    assert_eq!(rows.len(), DETERMINISTIC_CHECKS.len() + 2);

    let mut failed = record("gemini-2.5-pro");
    failed
        .errors
        .push(TraceError::new(CHECK_JUDGE_DISTILLATION, &eyre::eyre!("boom")));
    let rows = rows_from_record(&failed, true, "t");
    let err = rows.iter().find(|r| r.check_name == CHECK_JUDGE_DISTILLATION).unwrap();
    assert_eq!(err.error.as_deref(), Some("boom"));
    assert!(
        !rows.iter().any(|r| r.check_name == CHECK_JUDGE_CLAIMS),
        "a check that never ran leaves no row"
    );
}

#[test]
fn no_judge_writes_no_judge_rows() {
    let mut r = record("m");
    r.metrics.judge_claims = Some(vec![ClaimVerdict::Supported]);
    let rows = rows_from_record(&r, false, "t");
    assert_eq!(rows.len(), DETERMINISTIC_CHECKS.len());
}

#[test]
fn scores_persist_across_a_reopen_of_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("eval-cache.db");
    ScoreStore::open(&path)
        .unwrap()
        .put(&row("tr-1", "baseline", "", "m", CHECK_NOTE_SIZE, "h"))
        .unwrap();
    assert_eq!(ScoreStore::open(&path).unwrap().rows_for("tr-1").unwrap().len(), 1);
}

#[test]
fn the_score_table_shares_a_file_with_the_judgment_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("eval-cache.db");
    let _cache = crate::eval::cache::JudgmentCache::open(&path).unwrap();
    let store = ScoreStore::open(&path).unwrap();
    store
        .put(&row("tr-1", "baseline", "", "m", CHECK_NOTE_SIZE, "h"))
        .unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    let mut tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    tables.sort();
    assert_eq!(tables, vec!["claim_judgments", "distill_judgments", "eval_scores"]);
}
