use super::*;
use crate::eval::scores::{CHECK_NOTE_SIZE, ScoreRow};
use crate::eval::{CHECK_JUDGE_DISTILLATION, RUN_KIND_INJECT};
use vault::distilled::{Claim, Distilled};

const NOW: &str = "2026-10-17T12:00:00Z";

/// Stage `trace` as an article with one claim and a transcript source.
fn stage(root: &Path, trace: &str, claim: &str) {
    let dir = root.join(trace);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("envelope.yml"), "kind: article-url\n").unwrap();
    let distilled = Distilled {
        summary: format!("summary of {trace}"),
        claims: vec![Claim {
            text: claim.to_string(),
            ..Default::default()
        }],
        ..Default::default()
    };
    std::fs::write(dir.join("distilled.yml"), serde_yaml::to_string(&distilled).unwrap()).unwrap();
    std::fs::write(dir.join("transcript.md"), format!("source for {trace}: {claim}")).unwrap();
}

struct Env {
    _tmp: tempfile::TempDir,
    root: std::path::PathBuf,
    store: ScoreStore,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("stages");
    std::fs::create_dir_all(&root).unwrap();
    Env {
        _tmp: tmp,
        root,
        store: ScoreStore::open_memory().unwrap(),
    }
}

fn baseline_of(env: &Env) -> impl Fn(&str) -> Result<Option<CurrentBaseline>> + '_ {
    |trace| staged_baseline(&env.root, trace)
}

fn hash_of(env: &Env, trace: &str) -> String {
    staged_baseline(&env.root, trace).unwrap().unwrap().content_hash
}

fn verdict(trace: &str, run_kind: &str, run_id: &str, hash: &str, tool: &str, metric: &str, value: f64) -> Verdict {
    Verdict {
        trace: trace.to_string(),
        run_kind: run_kind.to_string(),
        run_id: run_id.to_string(),
        rubric_version: "v2".to_string(),
        content_hash: hash.to_string(),
        tool: tool.to_string(),
        tool_version: "1.0".to_string(),
        model: "model-x".to_string(),
        claim_id: None,
        metric: metric.to_string(),
        value,
        detail: None,
    }
}

fn claim_verdict(v: Verdict, claim: u64, detail: Value) -> Verdict {
    Verdict {
        claim_id: Some(json!(claim)),
        detail: Some(detail),
        ..v
    }
}

/// A judge row a run already has (what `--inject` or a candidate run wrote).
fn scored_row(trace: &str, run_kind: &str, run_id: &str, hash: &str) -> ScoreRow {
    ScoreRow {
        trace: trace.to_string(),
        run_kind: run_kind.to_string(),
        run_id: run_id.to_string(),
        kind: "article".to_string(),
        judge_model: "gemini-2.5-pro".to_string(),
        check_name: CHECK_JUDGE_DISTILLATION.to_string(),
        rubric_version: "v2".to_string(),
        content_hash: hash.to_string(),
        scored_at: NOW.to_string(),
        metrics_json: r#"{"claim_coverage":3}"#.to_string(),
        error: None,
    }
}

fn write_file(dir: &Path, lines: &[&str]) -> std::path::PathBuf {
    let path = dir.join("verdicts.jsonl");
    std::fs::write(&path, lines.join("\n")).unwrap();
    path
}

const GOOD: &str = r#"{"trace":"t1","run_kind":"baseline","run_id":"","rubric_version":"v2","content_hash":"h","tool":"minicheck","tool_version":"git-b58b9fa","model":"roberta-large","claim_id":1,"metric":"supported","value":0.91,"detail":{"label":1}}"#;

#[test]
fn a_verdict_line_from_the_tool_contract_parses() {
    let tmp = tempfile::tempdir().unwrap();
    let verdicts = read_verdicts(&write_file(tmp.path(), &[GOOD, ""])).unwrap();
    assert_eq!(verdicts.len(), 1);
    assert_eq!(verdicts[0].claim_id, Some(json!(1)));
    assert_eq!(verdicts[0].detail, Some(json!({"label": 1})));
}

#[test]
fn an_unknown_key_is_an_error_naming_its_line() {
    let tmp = tempfile::tempdir().unwrap();
    let drifted = GOOD.replace(r#""metric""#, r#""seconds":2,"metric""#);
    let err = read_verdicts(&write_file(tmp.path(), &[GOOD, &drifted])).unwrap_err();
    assert!(format!("{err:#}").contains("verdicts.jsonl:2"), "{err:#}");
    assert!(format!("{err:#}").contains("seconds"), "{err:#}");
}

#[test]
fn a_missing_contract_field_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let no_hash = GOOD.replace(r#""content_hash":"h","#, "");
    let err = read_verdicts(&write_file(tmp.path(), &[&no_hash])).unwrap_err();
    assert!(format!("{err:#}").contains("content_hash"), "{err:#}");
}

#[test]
fn an_empty_model_is_rejected_because_empty_means_a_deterministic_check() {
    let tmp = tempfile::tempdir().unwrap();
    let err = read_verdicts(&write_file(tmp.path(), &[&GOOD.replace("roberta-large", "")])).unwrap_err();
    assert!(format!("{err:#}").contains("empty model"), "{err:#}");
}

#[test]
fn a_tool_named_like_a_builtin_check_cannot_write_that_checks_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let forged = GOOD.replace("minicheck", "judge-claims");
    let err = read_verdicts(&write_file(tmp.path(), &[&forged])).unwrap_err();
    assert!(format!("{err:#}").contains("built-in check name"), "{err:#}");
}

#[test]
fn a_current_baseline_verdict_lands_with_the_tool_as_check_and_the_model_as_judge() {
    let e = env();
    stage(&e.root, "t1", "claim one");
    let h = hash_of(&e, "t1");
    let verdicts = vec![
        claim_verdict(
            verdict("t1", "baseline", "", &h, "minicheck", "supported", 0.9),
            1,
            json!({"label": 1}),
        ),
        verdict("t1", "baseline", "", &h, "deepeval", "summarization", 0.4),
    ];
    let outcome = ingest(&verdicts, &e.store, &baseline_of(&e), NOW).unwrap();
    assert_eq!(outcome.rejected, vec![]);
    assert_eq!((outcome.verdicts_loaded, outcome.rows_written), (2, 2));
    outcome.check().unwrap();

    let rows = e.store.rows_for("t1").unwrap();
    let checks: Vec<&str> = rows.iter().map(|r| r.check_name.as_str()).collect();
    assert_eq!(checks, vec!["deepeval", "minicheck"]);
    let minicheck = rows.iter().find(|r| r.check_name == "minicheck").unwrap();
    assert_eq!(
        (
            minicheck.judge_model.as_str(),
            minicheck.kind.as_str(),
            minicheck.error.clone()
        ),
        ("model-x", "article", None)
    );
    let metrics: Value = serde_json::from_str(&minicheck.metrics_json).unwrap();
    assert_eq!(metrics["tool_version"], "1.0");
    assert_eq!(metrics["claims"]["1"]["supported"]["value"], 0.9);
    assert_eq!(metrics["claims"]["1"]["supported"]["detail"]["label"], 1);
    let deepeval: Value = serde_json::from_str(&rows[0].metrics_json).unwrap();
    assert_eq!(deepeval["metrics"]["summarization"]["value"], 0.4);
}

#[test]
fn one_tools_claim_verdicts_fold_into_one_row() {
    let e = env();
    stage(&e.root, "t1", "claim one");
    let h = hash_of(&e, "t1");
    let base = verdict("t1", "baseline", "", &h, "minicheck", "supported", 0.9);
    let verdicts = vec![
        claim_verdict(base.clone(), 1, json!(null)),
        claim_verdict(Verdict { value: 0.2, ..base }, 2, json!(null)),
    ];
    let outcome = ingest(&verdicts, &e.store, &baseline_of(&e), NOW).unwrap();
    assert_eq!((outcome.verdicts_loaded, outcome.rows_written), (2, 1));
    let rows = e.store.rows_for("t1").unwrap();
    let metrics: Value = serde_json::from_str(&rows[0].metrics_json).unwrap();
    assert_eq!(metrics["claims"]["1"]["supported"]["value"], 0.9);
    assert_eq!(metrics["claims"]["2"]["supported"]["value"], 0.2);
}

#[test]
fn a_stale_baseline_hash_is_rejected_naming_the_trace_while_current_verdicts_still_load() {
    let e = env();
    stage(&e.root, "t-stale", "claim one");
    stage(&e.root, "t-fresh", "claim two");
    let fresh = hash_of(&e, "t-fresh");
    let verdicts = vec![
        verdict(
            "t-stale",
            "baseline",
            "",
            "0000stale0000000",
            "deepeval",
            "summarization",
            0.5,
        ),
        verdict("t-fresh", "baseline", "", &fresh, "deepeval", "summarization", 0.7),
    ];
    let outcome = ingest(&verdicts, &e.store, &baseline_of(&e), NOW).unwrap();

    assert_eq!(outcome.rows_written, 1);
    assert!(
        e.store.rows_for("t-stale").unwrap().is_empty(),
        "the stale verdict is not loaded"
    );
    assert_eq!(e.store.rows_for("t-fresh").unwrap().len(), 1);
    assert_eq!(outcome.rejected.len(), 1);
    assert_eq!(outcome.rejected[0].trace, "t-stale");
    assert!(outcome.rejected[0].reason.contains("stale content_hash"));
    assert!(outcome.rejection_lines()[0].contains("trace t-stale"));
    let err = outcome.check().unwrap_err().to_string();
    assert!(err.contains("t-stale") && !err.contains("t-fresh"), "{err}");
}

#[test]
fn re_distilling_a_trace_after_the_verdicts_were_written_makes_them_stale() {
    let e = env();
    stage(&e.root, "t1", "claim one");
    let old = hash_of(&e, "t1");
    stage(&e.root, "t1", "a different claim");
    let outcome = ingest(
        &[verdict("t1", "baseline", "", &old, "deepeval", "summarization", 0.5)],
        &e.store,
        &baseline_of(&e),
        NOW,
    )
    .unwrap();
    assert_eq!(outcome.rows_written, 0);
    assert!(outcome.check().is_err());
}

#[test]
fn a_baseline_verdict_for_a_trace_swept_from_staging_is_rejected() {
    let e = env();
    let outcome = ingest(
        &[verdict("gone", "baseline", "", "h", "deepeval", "summarization", 0.5)],
        &e.store,
        &baseline_of(&e),
        NOW,
    )
    .unwrap();
    assert!(outcome.rejected[0].reason.contains("no longer in staging"));
    assert!(outcome.check().unwrap_err().to_string().contains("gone"));
}

#[test]
fn a_candidate_or_inject_verdict_needs_a_run_scored_at_that_hash() {
    let e = env();
    e.store
        .put(&scored_row("t1", "candidate", "distill-video-aaaa", "h-cand"))
        .unwrap();
    let verdicts = vec![
        // right run, wrong (stale) hash
        verdict(
            "t1",
            "candidate",
            "distill-video-aaaa",
            "h-old",
            "minicheck",
            "supported",
            0.5,
        ),
        // a run that was never scored
        verdict(
            "t1",
            "inject",
            "inject-x:number-swap",
            "h-cand",
            "minicheck",
            "supported",
            0.5,
        ),
        // a good one
        verdict(
            "t1",
            "candidate",
            "distill-video-aaaa",
            "h-cand",
            "minicheck",
            "supported",
            0.5,
        ),
    ];
    let outcome = ingest(&verdicts, &e.store, &|_| Ok(None), NOW).unwrap();
    assert_eq!(outcome.rows_written, 1);
    assert_eq!(outcome.rejected.len(), 2);
    assert!(outcome.rejected.iter().all(|r| r.trace == "t1"));
}

#[test]
fn baseline_candidate_and_inject_verdicts_from_one_tool_land_in_three_distinct_rows() {
    let e = env();
    stage(&e.root, "t1", "claim one");
    let h = hash_of(&e, "t1");
    // The candidate and the injection were scored earlier; each run has its own
    // judge row at its own hash, and a mutant's hash is not the baseline's.
    e.store
        .put(&scored_row("t1", "candidate", "distill-article-beef", &h))
        .unwrap();
    let mut inject_row = scored_row("t1", RUN_KIND_INJECT, "inject-20261017T135831Z:number-swap", "h-mutant");
    inject_row.kind = "article".to_string();
    e.store.put(&inject_row).unwrap();
    e.store.put(&scored_row("t1", "baseline", "", &h)).unwrap();
    let before = e.store.rows_for("t1").unwrap();

    let verdicts = vec![
        verdict("t1", "baseline", "", &h, "minicheck", "supported", 0.1),
        verdict(
            "t1",
            "candidate",
            "distill-article-beef",
            &h,
            "minicheck",
            "supported",
            0.5,
        ),
        verdict(
            "t1",
            "inject",
            "inject-20261017T135831Z:number-swap",
            "h-mutant",
            "minicheck",
            "supported",
            0.9,
        ),
    ];
    let outcome = ingest(&verdicts, &e.store, &baseline_of(&e), NOW).unwrap();
    assert_eq!((outcome.rows_written, outcome.rejected.len()), (3, 0));

    let rows = e.store.rows_for("t1").unwrap();
    let minicheck: Vec<(&str, &str, &str)> = rows
        .iter()
        .filter(|r| r.check_name == "minicheck")
        .map(|r| (r.run_kind.as_str(), r.run_id.as_str(), r.content_hash.as_str()))
        .collect();
    assert_eq!(
        minicheck,
        vec![
            ("baseline", "", h.as_str()),
            ("candidate", "distill-article-beef", h.as_str()),
            ("inject", "inject-20261017T135831Z:number-swap", "h-mutant"),
        ]
    );
    let value_of = |kind: &str| -> f64 {
        let r = rows
            .iter()
            .find(|r| r.check_name == "minicheck" && r.run_kind == kind)
            .unwrap();
        let m: Value = serde_json::from_str(&r.metrics_json).unwrap();
        m["metrics"]["supported"]["value"].as_f64().unwrap()
    };
    assert_eq!(
        (value_of("baseline"), value_of("candidate"), value_of("inject")),
        (0.1, 0.5, 0.9)
    );
    // None of the existing judge rows was touched.
    let judge_rows: Vec<&ScoreRow> = rows
        .iter()
        .filter(|r| r.check_name == CHECK_JUDGE_DISTILLATION)
        .collect();
    assert_eq!(judge_rows.len(), before.len());
    assert!(judge_rows.iter().all(|r| r.metrics_json == r#"{"claim_coverage":3}"#));
}

#[test]
fn re_ingesting_the_same_file_overwrites_its_rows_instead_of_adding() {
    let e = env();
    stage(&e.root, "t1", "claim one");
    let h = hash_of(&e, "t1");
    let v = [verdict("t1", "baseline", "", &h, "deepeval", "summarization", 0.5)];
    ingest(&v, &e.store, &baseline_of(&e), NOW).unwrap();
    ingest(&v, &e.store, &baseline_of(&e), NOW).unwrap();
    assert_eq!(e.store.rows_for("t1").unwrap().len(), 1);
}

#[test]
fn the_same_metric_twice_for_one_claim_is_an_error_and_writes_nothing() {
    let e = env();
    stage(&e.root, "t1", "claim one");
    stage(&e.root, "t2", "claim two");
    let (h1, h2) = (hash_of(&e, "t1"), hash_of(&e, "t2"));
    let verdicts = vec![
        verdict("t1", "baseline", "", &h1, "deepeval", "summarization", 0.5),
        verdict("t2", "baseline", "", &h2, "deepeval", "summarization", 0.5),
        verdict("t2", "baseline", "", &h2, "deepeval", "summarization", 0.6),
    ];
    let err = ingest(&verdicts, &e.store, &baseline_of(&e), NOW).unwrap_err();
    assert!(err.to_string().contains("twice"), "{err}");
    assert!(e.store.rows_for("t1").unwrap().is_empty(), "no half-loaded file");
}

#[test]
fn two_tool_versions_in_one_row_are_an_error() {
    let e = env();
    stage(&e.root, "t1", "claim one");
    let h = hash_of(&e, "t1");
    let first = claim_verdict(
        verdict("t1", "baseline", "", &h, "minicheck", "supported", 0.5),
        1,
        json!(null),
    );
    let second = Verdict {
        tool_version: "2.0".to_string(),
        ..claim_verdict(
            verdict("t1", "baseline", "", &h, "minicheck", "supported", 0.5),
            2,
            json!(null),
        )
    };
    let err = ingest(&[first, second], &e.store, &baseline_of(&e), NOW).unwrap_err();
    assert!(err.to_string().contains("two tool versions"), "{err}");
}

#[test]
fn a_clean_outcome_passes_check() {
    IngestOutcome::default().check().unwrap();
}

#[test]
fn a_verdict_row_does_not_cover_a_trace_for_the_required_checks() {
    // A tool row is extra signal, never one of the required checks.
    let e = env();
    stage(&e.root, "t1", "claim one");
    let h = hash_of(&e, "t1");
    ingest(
        &[verdict("t1", "baseline", "", &h, "deepeval", "summarization", 0.5)],
        &e.store,
        &baseline_of(&e),
        NOW,
    )
    .unwrap();
    let required = crate::eval::scores::required_checks(&["model-x".to_string()]);
    assert!(!e.store.is_covered("t1", &h, &required).unwrap());
    assert!(required.iter().any(|c| c.check_name == CHECK_NOTE_SIZE));
}
