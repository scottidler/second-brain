use super::*;
use std::collections::BTreeMap;

fn jt(title: &str) -> JudgeText {
    JudgeText {
        title: title.to_string(),
        text: format!("body of {title}"),
        content_hash: format!("h-{title}"),
        truncated: false,
    }
}

fn ranked(pairs: &[(&str, &[&str])]) -> BTreeMap<String, Vec<String>> {
    pairs
        .iter()
        .map(|(label, list)| (label.to_string(), list.iter().map(|s| s.to_string()).collect()))
        .collect()
}

fn texts(pairs: &[(&str, &str)]) -> BTreeMap<String, JudgeText> {
    pairs
        .iter()
        .map(|(path, title)| (path.to_string(), jt(title)))
        .collect()
}

/// Two-query fixture: q1's graph-hybrid surfaces a relevant note (`d`) that
/// hybrid and the ablation miss (so graph-hybrid lifts and the fact layer is
/// "touched"); q2 is identical across modes (no lift, no fact touch) and carries
/// a calibration label the judge agrees with.
fn fixture() -> (Queries, Vec<QueryRun>) {
    let queries = Queries {
        queries: vec![
            EvalQuery {
                id: "q1".into(),
                query: "alpha".into(),
                tags: None,
                calibration: None,
            },
            EvalQuery {
                id: "q2".into(),
                query: "beta".into(),
                tags: None,
                calibration: Some(BTreeMap::from([("notes/a.md".to_string(), 3u8)])),
            },
        ],
    };
    let runs = vec![
        QueryRun {
            ranked: ranked(&[
                ("bm25", &["notes/a.md", "notes/b.md"]),
                ("vector", &["notes/a.md", "notes/c.md"]),
                ("hybrid", &["notes/a.md", "notes/b.md"]),
                ("graph", &["notes/a.md", "notes/d.md"]),
                ("graph-hybrid", &["notes/a.md", "notes/d.md"]),
                (ABLATION_LABEL, &["notes/a.md", "notes/b.md"]),
            ]),
            texts: texts(&[
                ("notes/a.md", "A"),
                ("notes/b.md", "B"),
                ("notes/c.md", "C"),
                ("notes/d.md", "D"),
            ]),
        },
        QueryRun {
            ranked: ranked(&[
                ("bm25", &["notes/a.md", "notes/e.md"]),
                ("vector", &["notes/a.md", "notes/e.md"]),
                ("hybrid", &["notes/a.md", "notes/e.md"]),
                ("graph", &["notes/a.md", "notes/e.md"]),
                ("graph-hybrid", &["notes/a.md", "notes/e.md"]),
                (ABLATION_LABEL, &["notes/a.md", "notes/e.md"]),
            ]),
            texts: texts(&[("notes/a.md", "A"), ("notes/e.md", "E")]),
        },
    ];
    (queries, runs)
}

fn judge() -> MockJudge {
    MockJudge::new(0)
        .with("alpha", "A", 3)
        .with("alpha", "B", 0)
        .with("alpha", "C", 2)
        .with("alpha", "D", 2)
        .with("beta", "A", 3)
        .with("beta", "E", 2)
}

fn unwrap_report(outcome: EvalOutcome) -> EvalReport {
    match outcome {
        EvalOutcome::Report(r) => *r,
        EvalOutcome::CalibrationSheet(_) => panic!("expected a report, got a calibration sheet"),
    }
}

#[test]
fn evaluate_produces_all_mode_rows_and_graph_lift() {
    let (queries, runs) = fixture();
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let opts = EvalOpts::default();
    let report = unwrap_report(evaluate(&queries, &runs, &cache, &judge(), &opts).expect("evaluate"));

    assert_eq!(report.total_queries, 2);
    // five standard modes + ablation + the live configured pipeline
    assert_eq!(report.modes.len(), 7);
    let labels: Vec<&str> = report.modes.iter().map(|m| m.mode.as_str()).collect();
    assert_eq!(
        labels,
        vec![
            "bm25",
            "vector",
            "hybrid",
            "graph",
            "graph-hybrid",
            ABLATION_LABEL,
            CONFIGURED_LABEL
        ]
    );

    // q1: graph-hybrid surfaces the relevant `d` that hybrid misses -> positive lift.
    assert!(
        report.lift_ndcg > 0.0,
        "expected graph-hybrid nDCG lift, got {}",
        report.lift_ndcg
    );
    // every pooled note judged: q1 {a,b,c,d}=4 + q2 {a,e}=2
    assert_eq!(report.total_judgments, 6);
}

#[test]
fn evaluate_reports_fact_ablation_coverage() {
    let (queries, runs) = fixture();
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let report = unwrap_report(evaluate(&queries, &runs, &cache, &judge(), &EvalOpts::default()).expect("evaluate"));
    // only q1's graph-hybrid differs from its ablation -> 1 of 2 queries touched.
    assert_eq!(report.ablation.queries_touching_fact, 1);
    assert_eq!(report.ablation.total_queries, 2);
    assert!(!report.ablation.inconclusive);
    // fact edge brought in relevant `d` -> graph-hybrid beats the no-fact ablation.
    assert!(report.ablation.ndcg_lift_vs_ablation > 0.0);
}

#[test]
fn evaluate_computes_calibration_panel() {
    let (queries, runs) = fixture();
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let report = unwrap_report(evaluate(&queries, &runs, &cache, &judge(), &EvalOpts::default()).expect("evaluate"));
    let c = report.calibration.expect("calibration present (q2 has a label)");
    assert_eq!(c.pairs, 1); // one labeled pair: human 3 vs judge 3
    assert!((c.exact_pct - 1.0).abs() < 1e-9);
    assert!(c.trustworthy);
}

#[test]
fn evaluate_caches_judgments_across_runs() {
    let (queries, runs) = fixture();
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let opts = EvalOpts::default();
    // First run populates the cache.
    let _ = evaluate(&queries, &runs, &cache, &judge(), &opts).expect("run1");
    // Second run with a judge that would return different scores; cache should win
    // (so the report matches the first run's judgments, not the new judge).
    let other = MockJudge::new(0); // everything irrelevant if consulted
    let report = unwrap_report(evaluate(&queries, &runs, &cache, &other, &opts).expect("run2"));
    // graph lift still present because cached judgments (D=2 etc.) were reused.
    assert!(report.lift_ndcg > 0.0, "cache should preserve first-run judgments");
}

#[test]
fn emit_calibration_writes_sheet_and_short_circuits() {
    let (queries, runs) = fixture();
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let tmp = tempfile::NamedTempFile::new().expect("tmp");
    let opts = EvalOpts {
        emit_calibration: Some(tmp.path().to_path_buf()),
        ..EvalOpts::default()
    };
    match evaluate(&queries, &runs, &cache, &judge(), &opts).expect("evaluate") {
        EvalOutcome::CalibrationSheet(p) => assert_eq!(p, tmp.path()),
        EvalOutcome::Report(_) => panic!("expected a calibration sheet"),
    }
    let written = std::fs::read_to_string(tmp.path()).expect("read sheet");
    // q2 is the calibration query; its pool {a,e} -> 2 rows with a `human` field.
    assert!(written.contains("query_id: q2"));
    assert!(written.contains("human:"));
}

/// The production relevance judge carries
/// `oracle.yml`'s `llm.api-keys` into the fabric child it runs.
#[test]
fn fabric_judge_exports_the_configured_key_map_on_its_fabric_child() {
    let dir = tempfile::tempdir().expect("tempdir");
    let anthropic = dir.path().join("anthropic");
    let gemini = dir.path().join("gemini");
    std::fs::write(&anthropic, "anthropic-from-config").expect("write");
    std::fs::write(&gemini, "gemini-from-config").expect("write");
    let yaml = format!(
        "llm:\n  api-keys:\n    anthropic: {}\n    gemini: {}\n",
        anthropic.display(),
        gemini.display()
    );
    let config: Config = serde_yaml::from_str(&yaml).expect("parse oracle.yml");
    let judge = fabric_judge(&config, &EvalOpts::default());
    assert_eq!(judge.api_keys, config.llm.api_keys);
    let cmd = judge.command();
    let env = |name: &str| {
        cmd.get_envs()
            .find(|(k, _)| *k == std::ffi::OsStr::new(name))
            .and_then(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
    };
    assert_eq!(env("ANTHROPIC_API_KEY").as_deref(), Some("anthropic-from-config"));
    assert_eq!(env("GEMINI_API_KEY").as_deref(), Some("gemini-from-config"));
}

/// A relevance judge that fails every call.
struct BrokenJudge;

impl RelevanceJudge for BrokenJudge {
    fn judge(&self, _query: &str, _title: &str, _text: &str) -> Result<u8> {
        eyre::bail!("fabric -p judge-relevance failed: no key")
    }
}

#[test]
fn judge_errors_are_counted_and_reported() {
    let (queries, runs) = fixture();
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let outcome = evaluate(&queries, &runs, &cache, &BrokenJudge, &EvalOpts::default()).expect("evaluate");
    let report = unwrap_report(outcome);
    assert_eq!(report.total_judgments, 0);
    assert!(report.judge_errors > 0);
    assert!(
        report
            .render()
            .contains(&format!("{} judge errors", report.judge_errors))
    );
}
