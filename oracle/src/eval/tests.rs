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
            latency_ms: BTreeMap::from([("vector".to_string(), 30.0), (CONFIGURED_LABEL.to_string(), 120.0)]),
            rerank: Some(RerankRun::Ran),
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
            latency_ms: BTreeMap::from([("vector".to_string(), 10.0), (CONFIGURED_LABEL.to_string(), 80.0)]),
            rerank: Some(RerankRun::Ran),
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
    let report = unwrap_report(evaluate(&queries, &runs, None, &cache, &judge(), &opts).expect("evaluate"));

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
    let report =
        unwrap_report(evaluate(&queries, &runs, None, &cache, &judge(), &EvalOpts::default()).expect("evaluate"));
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
    let report =
        unwrap_report(evaluate(&queries, &runs, None, &cache, &judge(), &EvalOpts::default()).expect("evaluate"));
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
    let _ = evaluate(&queries, &runs, None, &cache, &judge(), &opts).expect("run1");
    // Second run with a judge that would return different scores; cache should win
    // (so the report matches the first run's judgments, not the new judge).
    let other = MockJudge::new(0); // everything irrelevant if consulted
    let report = unwrap_report(evaluate(&queries, &runs, None, &cache, &other, &opts).expect("run2"));
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
    match evaluate(&queries, &runs, None, &cache, &judge(), &opts).expect("evaluate") {
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

/// A judge failure fails the run instead of being skipped (which scored the
/// note 0 on every list that held it). Inverts the old
/// `judge_errors_are_counted_and_reported`.
#[test]
fn judge_errors_fail_the_run() {
    let (queries, runs) = fixture();
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let err = match evaluate(&queries, &runs, None, &cache, &BrokenJudge, &EvalOpts::default()) {
        Ok(_) => panic!("a judge failure must fail the run"),
        Err(e) => format!("{e:#}"),
    };
    assert!(err.contains("6 pooled note(s) have no judgment"), "{err}");
    assert!(err.contains("no key"), "the judge's error is named: {err}");
}

/// A pooled note with no judge text (in neither arm's index) is a missing
/// judgment too.
#[test]
fn pooled_note_without_judge_text_fails_the_run() {
    let (queries, mut runs) = fixture();
    runs[0].texts.remove("notes/d.md");
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let err = match evaluate(&queries, &runs, None, &cache, &judge(), &EvalOpts::default()) {
        Ok(_) => panic!("an unjudgeable pooled note must fail the run"),
        Err(e) => format!("{e:#}"),
    };
    assert!(err.contains("q1 / notes/d.md: no judge text"), "{err}");
}

/// Each target's latency comes from that target's own samples, and only the
/// configured target carries a rerank tally.
#[test]
fn evaluate_reports_latency_and_rerank_per_target() {
    let (queries, runs) = fixture();
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let report =
        unwrap_report(evaluate(&queries, &runs, None, &cache, &judge(), &EvalOpts::default()).expect("evaluate"));
    let row = |label: &str| report.modes.iter().find(|m| m.mode == label).expect(label);

    let vector = row("vector");
    assert_eq!(
        (vector.latency.p50_ms, vector.latency.p95_ms, vector.latency.n),
        (10.0, 30.0, 2)
    );
    assert_eq!(vector.rerank, None);
    // bm25 has no samples in the fixture.
    assert_eq!(row("bm25").latency.n, 0);

    let configured = row(CONFIGURED_LABEL);
    assert_eq!((configured.latency.p50_ms, configured.latency.p95_ms), (80.0, 120.0));
    assert_eq!(
        configured.rerank,
        Some(RerankTally {
            ran: 2,
            ..RerankTally::default()
        })
    );
    assert!(report.render().contains("ran 2/2"));
}

/// A rerank-on run whose stage was latched off is reported as not run, never
/// as rerank on.
#[test]
fn evaluate_flags_a_latched_off_rerank() {
    let (queries, mut runs) = fixture();
    for run in &mut runs {
        run.rerank = Some(RerankRun::Disabled);
    }
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let out = unwrap_report(evaluate(&queries, &runs, None, &cache, &judge(), &EvalOpts::default()).expect("evaluate"))
        .render();
    assert!(out.contains("NOT RUN 2/2"), "{out}");
    assert!(out.contains("NOT a rerank-on result"), "{out}");
    assert!(!out.contains("ran 2/2"), "{out}");
}

// ---- paired A/B ----------------------------------------------------------

fn paired_opts() -> EvalOpts {
    EvalOpts {
        compare_config: Some(PathBuf::from("/arm/b.yml")),
        ..EvalOpts::default()
    }
}

fn unwrap_paired(outcome: EvalOutcome) -> (EvalReport, PairedReport) {
    let report = unwrap_report(outcome);
    let paired = report.paired.clone().expect("a paired run carries the paired table");
    (report, paired)
}

/// Six queries so both splits hold several; each runs `vector` and `configured`.
/// Arm A puts the relevant note `r` second; `b_first` decides, per query,
/// whether arm B lifts it to first.
fn paired_fixture(b_first: &[bool]) -> (Queries, Vec<QueryRun>, Vec<QueryRun>) {
    let n = b_first.len();
    let queries = Queries {
        queries: (0..n)
            .map(|i| EvalQuery {
                id: format!("p{i}"),
                query: format!("query {i}"),
                tags: None,
                calibration: None,
            })
            .collect(),
    };
    let arm = |lift: &dyn Fn(usize) -> bool| -> Vec<QueryRun> {
        (0..n)
            .map(|i| {
                let configured: &[&str] = if lift(i) {
                    &["notes/r.md", "notes/x.md"]
                } else {
                    &["notes/x.md", "notes/r.md"]
                };
                QueryRun {
                    ranked: ranked(&[
                        ("vector", &["notes/x.md", "notes/r.md"]),
                        (CONFIGURED_LABEL, configured),
                    ]),
                    texts: BTreeMap::new(),
                    latency_ms: BTreeMap::from([("vector".to_string(), 10.0), (CONFIGURED_LABEL.to_string(), 50.0)]),
                    rerank: Some(RerankRun::Off),
                }
            })
            .collect()
    };
    let mut a = arm(&|_| false);
    let b = arm(&|i| b_first[i]);
    for run in &mut a {
        run.texts = texts(&[("notes/r.md", "R"), ("notes/x.md", "X"), ("notes/z.md", "Z")]);
    }
    (queries, a, b)
}

/// Relevant `r` (3), irrelevant `x`, and `z`, which only an added target
/// surfaces, also relevant.
fn paired_judge(n: usize) -> MockJudge {
    (0..n).fold(MockJudge::new(0), |j, i| {
        let q = format!("query {i}");
        j.with(&q, "R", 3).with(&q, "X", 0).with(&q, "Z", 3)
    })
}

fn row<'a>(p: &'a PairedReport, target: &str, split: Split) -> &'a PairedRow {
    p.rows
        .iter()
        .find(|r| r.target == target && r.split == split)
        .unwrap_or_else(|| panic!("no {target}/{} row", split.label()))
}

#[test]
fn compare_same_arm_zero_delta() {
    let lifts = [true, false, true, true, false, true];
    let (queries, a, _) = paired_fixture(&lifts);
    let b = a.clone();
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let outcome = evaluate(
        &queries,
        &a,
        Some(&b),
        &cache,
        &paired_judge(lifts.len()),
        &paired_opts(),
    )
    .expect("evaluate");
    let (_, paired) = unwrap_paired(outcome);
    assert_eq!(paired.rows.len(), mode_labels().len() * 2, "every target, both splits");
    for r in &paired.rows {
        assert_eq!(r.delta, 0.0, "{} {}", r.target, r.split.label());
        assert_eq!(r.ci, (0.0, 0.0), "{} {}", r.target, r.split.label());
        assert_eq!(r.verdict, Verdict::Insufficient, "zero variance never ships");
    }
    let rendered = paired.render();
    assert!(rendered.contains("+0.0000"), "{rendered}");
}

#[test]
fn compare_target_set_does_not_rebase() {
    let lifts = [true, false, true, true, false, true, true, false];
    let n = lifts.len();
    let (queries, a, b) = paired_fixture(&lifts);
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let base = evaluate(&queries, &a, Some(&b), &cache, &paired_judge(n), &paired_opts()).expect("base");
    let (base_report, base_paired) = unwrap_paired(base);

    // Add a target (`graph`) on both arms that surfaces a relevant note no
    // other target holds.
    let add = |runs: &[QueryRun]| -> Vec<QueryRun> {
        runs.iter()
            .cloned()
            .map(|mut r| {
                r.ranked.insert("graph".into(), vec!["notes/z.md".into()]);
                r
            })
            .collect()
    };
    let (a2, b2) = (add(&a), add(&b));
    let grown = evaluate(&queries, &a2, Some(&b2), &cache, &paired_judge(n), &paired_opts()).expect("grown");
    let (grown_report, grown_paired) = unwrap_paired(grown);

    for split in Split::ALL {
        for target in ["vector", CONFIGURED_LABEL] {
            let (before, after) = (row(&base_paired, target, split), row(&grown_paired, target, split));
            assert_eq!(
                (before.ndcg_a, before.ndcg_b, before.delta, before.ci, before.n),
                (after.ndcg_a, after.ndcg_b, after.delta, after.ci, after.n),
                "{target}/{} paired numbers moved when a target was added",
                split.label()
            );
        }
    }
    assert!(
        row(&base_paired, CONFIGURED_LABEL, Split::of("p0")).delta > 0.0,
        "the fixture's configured target has a real delta to protect"
    );

    // The legacy single-arm report shares one pool across targets, so the
    // added target's relevant note re-bases every other target.
    let legacy = |r: &EvalReport, label: &str| r.modes.iter().find(|m| m.mode == label).expect(label).means.ndcg;
    for target in ["vector", CONFIGURED_LABEL] {
        assert!(
            legacy(&grown_report, target) < legacy(&base_report, target),
            "legacy {target} should re-base: {} -> {}",
            legacy(&base_report, target),
            legacy(&grown_report, target)
        );
    }
}

#[test]
fn compare_missing_judgment_fails() {
    let lifts = [true, false, true, true];
    let (queries, a, mut b) = paired_fixture(&lifts);
    // A note only arm B surfaces, so only the paired pool holds it.
    b[1].ranked.insert("vector".into(), vec!["notes/z.md".into()]);
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let judge = paired_judge(lifts.len()).failing("query 1", "Z");
    let err = match evaluate(&queries, &a, Some(&b), &cache, &judge, &paired_opts()) {
        Ok(_) => panic!("a dropped judgment must fail the paired run"),
        Err(e) => format!("{e:#}"),
    };
    assert!(err.contains("1 pooled note(s) have no judgment"), "{err}");
    assert!(err.contains("p1 / notes/z.md"), "{err}");
}

#[test]
fn compare_target_on_one_arm_only_fails() {
    let (queries, a, mut b) = paired_fixture(&[true, false]);
    b[0].ranked.remove("vector");
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    assert!(evaluate(&queries, &a, Some(&b), &cache, &paired_judge(2), &paired_opts()).is_err());
}

#[test]
fn single_arm_run_has_no_paired_table() {
    let (queries, runs) = fixture();
    let cache = cache::JudgmentCache::open_memory().expect("cache");
    let report =
        unwrap_report(evaluate(&queries, &runs, None, &cache, &judge(), &EvalOpts::default()).expect("evaluate"));
    assert!(report.paired.is_none());
    assert!(!report.render().contains("paired A/B"));
}
