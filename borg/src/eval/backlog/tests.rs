use super::*;
use crate::config::DEFAULT_CLAIMS_SOURCE_MAX_CHARS;
use crate::eval::judge::{AxisScores, ClaimVerdict, MockClaimJudge, MockJudge};
use crate::eval::scores::{CHECK_NOTE_SIZE, DETERMINISTIC_CHECKS, ScoreRow, required_checks};
use crate::eval::{CHECK_JUDGE_CLAIMS, CHECK_JUDGE_DISTILLATION, RUN_KIND_BASELINE};
use crate::receipts;
use vault::distilled::{Claim, Distilled};
use vault::receipts::ReceiptKind;
use vault::schema::Method;

const MODEL: &str = "gemini-2.5-pro";

fn models() -> Vec<String> {
    vec![MODEL.to_string()]
}

fn axes() -> AxisScores {
    AxisScores {
        claim_coverage: 3,
        anchor_validity: 3,
        summary_faithfulness: 3,
    }
}

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
    std::fs::write(
        dir.join("transcript.md"),
        format!("source text for {trace}, which says {claim}"),
    )
    .unwrap();
}

/// A succeeded receipt for `trace`, terminal at `terminal_at`.
fn succeed(conn: &rusqlite::Connection, trace: &str, terminal_at: &str) {
    receipts::record_received(conn, trace, Method::Cli, ReceiptKind::Url, "https://example.com/x").unwrap();
    receipts::mark_succeeded(conn, trace, "inbox/x.md", false).unwrap();
    conn.execute(
        "UPDATE receipts SET terminal_at = ?1 WHERE trace_id = ?2",
        rusqlite::params![terminal_at, trace],
    )
    .unwrap();
}

struct Env {
    _tmp: tempfile::TempDir,
    root: std::path::PathBuf,
    receipts: rusqlite::Connection,
    store: ScoreStore,
    cache: cache::JudgmentCache,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("stages");
    std::fs::create_dir_all(&root).unwrap();
    Env {
        _tmp: tmp,
        root,
        receipts: receipts::open_memory().unwrap(),
        store: ScoreStore::open_memory().unwrap(),
        cache: cache::JudgmentCache::open_memory().unwrap(),
    }
}

fn current_hash(env: &Env, trace: &str) -> String {
    let fx = load_staged_trace(&env.root, trace).unwrap().unwrap();
    judged_input(&fx).content_hash
}

/// A baseline row (clean) for `check` at `hash`.
fn put_row(env: &Env, trace: &str, hash: &str, check: &crate::eval::scores::RequiredCheck, error: Option<&str>) {
    env.store
        .put(&ScoreRow {
            trace: trace.to_string(),
            run_kind: RUN_KIND_BASELINE.to_string(),
            run_id: String::new(),
            kind: "article".to_string(),
            judge_model: check.judge_model.clone(),
            check_name: check.check_name.clone(),
            rubric_version: check.rubric_version.clone(),
            content_hash: hash.to_string(),
            scored_at: "2026-10-17T00:00:00Z".to_string(),
            metrics_json: "{}".to_string(),
            error: error.map(str::to_string),
        })
        .unwrap();
}

fn cover_fully(env: &Env, trace: &str, required: &[crate::eval::scores::RequiredCheck]) {
    let hash = current_hash(env, trace);
    for check in required {
        put_row(env, trace, &hash, check, None);
    }
}

#[test]
fn backlog_lists_uncovered_traces_oldest_terminal_first() {
    let env = env();
    for (trace, at) in [("tr-new", "2026-10-17T10:00:00Z"), ("tr-old", "2026-10-10T10:00:00Z")] {
        stage(&env.root, trace, "a claim");
        succeed(&env.receipts, trace, at);
    }
    let required = required_checks(&models());
    let backlog = find_backlog(
        &env.receipts,
        &env.store,
        &env.root,
        &required,
        None,
        DEFAULT_CLAIMS_SOURCE_MAX_CHARS,
    )
    .unwrap();
    assert_eq!(backlog.traces, vec!["tr-old", "tr-new"]);
}

#[test]
fn a_trace_whose_only_current_row_for_a_required_check_is_an_error_is_listed() {
    let env = env();
    stage(&env.root, "tr-1", "a claim");
    succeed(&env.receipts, "tr-1", "2026-10-17T10:00:00Z");
    let required = required_checks(&models());
    let hash = current_hash(&env, "tr-1");
    for check in &required {
        let err = (check.check_name == CHECK_JUDGE_CLAIMS).then_some("429 rate limited");
        put_row(&env, "tr-1", &hash, check, err);
    }
    let backlog = find_backlog(
        &env.receipts,
        &env.store,
        &env.root,
        &required,
        None,
        DEFAULT_CLAIMS_SOURCE_MAX_CHARS,
    )
    .unwrap();
    assert_eq!(backlog.traces, vec!["tr-1"], "an error row does not cover the trace");

    put_row(
        &env,
        "tr-1",
        &hash,
        required.iter().find(|c| c.check_name == CHECK_JUDGE_CLAIMS).unwrap(),
        None,
    );
    let backlog = find_backlog(
        &env.receipts,
        &env.store,
        &env.root,
        &required,
        None,
        DEFAULT_CLAIMS_SOURCE_MAX_CHARS,
    )
    .unwrap();
    assert!(backlog.traces.is_empty(), "the retry that succeeds covers it");
}

#[test]
fn a_trace_scored_by_a_judge_outside_the_required_set_is_listed() {
    let env = env();
    stage(&env.root, "tr-1", "a claim");
    succeed(&env.receipts, "tr-1", "2026-10-17T10:00:00Z");
    cover_fully(&env, "tr-1", &required_checks(&["some-other-judge".to_string()]));
    let backlog = find_backlog(
        &env.receipts,
        &env.store,
        &env.root,
        &required_checks(&models()),
        None,
        DEFAULT_CLAIMS_SOURCE_MAX_CHARS,
    )
    .unwrap();
    assert_eq!(backlog.traces, vec!["tr-1"]);
}

#[test]
fn a_trace_with_an_older_content_hash_row_but_none_for_its_current_hash_is_listed() {
    let env = env();
    stage(&env.root, "tr-1", "a claim");
    succeed(&env.receipts, "tr-1", "2026-10-17T10:00:00Z");
    let required = required_checks(&models());
    for check in &required {
        put_row(&env, "tr-1", "hash-of-an-earlier-note", check, None);
    }
    let backlog = find_backlog(
        &env.receipts,
        &env.store,
        &env.root,
        &required,
        None,
        DEFAULT_CLAIMS_SOURCE_MAX_CHARS,
    )
    .unwrap();
    assert_eq!(backlog.traces, vec!["tr-1"]);
}

#[test]
fn a_fully_covered_trace_is_not_in_the_backlog_and_re_distilling_it_puts_it_back() {
    let env = env();
    stage(&env.root, "tr-1", "the first claim");
    succeed(&env.receipts, "tr-1", "2026-10-17T10:00:00Z");
    let required = required_checks(&models());
    cover_fully(&env, "tr-1", &required);
    assert!(
        find_backlog(
            &env.receipts,
            &env.store,
            &env.root,
            &required,
            None,
            DEFAULT_CLAIMS_SOURCE_MAX_CHARS
        )
        .unwrap()
        .traces
        .is_empty()
    );
    // The staged note changes: its distill hash changes, the old rows go stale.
    stage(&env.root, "tr-1", "a different claim");
    assert_eq!(
        find_backlog(
            &env.receipts,
            &env.store,
            &env.root,
            &required,
            None,
            DEFAULT_CLAIMS_SOURCE_MAX_CHARS
        )
        .unwrap()
        .traces,
        vec!["tr-1"]
    );
}

#[test]
fn only_succeeded_receipts_with_a_staged_source_are_owed_and_the_rest_are_counted() {
    let env = env();
    stage(&env.root, "tr-ok", "a claim");
    succeed(&env.receipts, "tr-ok", "2026-10-17T10:00:00Z");
    succeed(&env.receipts, "tr-swept", "2026-10-17T10:00:00Z");
    stage(&env.root, "tr-failed", "a claim");
    receipts::record_received(&env.receipts, "tr-failed", Method::Cli, ReceiptKind::Url, "u").unwrap();
    succeed(&env.receipts, "../escape", "2026-10-17T10:00:00Z");
    let backlog = find_backlog(
        &env.receipts,
        &env.store,
        &env.root,
        &required_checks(&models()),
        None,
        DEFAULT_CLAIMS_SOURCE_MAX_CHARS,
    )
    .unwrap();
    assert_eq!(backlog.traces, vec!["tr-ok"]);
    assert_eq!(backlog.not_staged, 2, "swept trace and the non-bare trace id");
}

#[test]
fn an_unparseable_staged_trace_is_reported_not_dropped_or_fatal() {
    let env = env();
    stage(&env.root, "tr-bad", "a claim");
    std::fs::write(env.root.join("tr-bad/distilled.yml"), ": : not yaml [").unwrap();
    stage(&env.root, "tr-ok", "a claim");
    succeed(&env.receipts, "tr-bad", "2026-10-10T10:00:00Z");
    succeed(&env.receipts, "tr-ok", "2026-10-17T10:00:00Z");
    let backlog = find_backlog(
        &env.receipts,
        &env.store,
        &env.root,
        &required_checks(&models()),
        None,
        DEFAULT_CLAIMS_SOURCE_MAX_CHARS,
    )
    .unwrap();
    assert_eq!(backlog.traces, vec!["tr-ok"]);
    assert_eq!(backlog.unreadable.len(), 1);
    assert_eq!(backlog.unreadable[0].0, "tr-bad");
}

#[test]
fn limit_takes_the_oldest_and_leaves_the_rest_for_the_next_run() {
    let env = env();
    for (trace, at) in [
        ("tr-c", "2026-10-03T00:00:00Z"),
        ("tr-a", "2026-10-01T00:00:00Z"),
        ("tr-b", "2026-10-02T00:00:00Z"),
    ] {
        stage(&env.root, trace, "a claim");
        succeed(&env.receipts, trace, at);
    }
    let judges = |_: &EvalOpts| -> Result<(Box<dyn DistillationJudge>, Box<dyn ClaimJudge>)> {
        Ok((
            Box::new(MockJudge::new(axes())),
            Box::new(MockClaimJudge::new(ClaimVerdict::Supported)),
        ))
    };
    let run = |limit| {
        run_unscored(
            &env.receipts,
            &env.store,
            &env.cache,
            &EvalOpts::default(),
            &env.root,
            &models(),
            limit,
            &judges,
        )
        .unwrap()
    };
    let first = run(Some(2));
    assert_eq!(first.selected, 2);
    let required = required_checks(&models());
    let left = find_backlog(
        &env.receipts,
        &env.store,
        &env.root,
        &required,
        None,
        DEFAULT_CLAIMS_SOURCE_MAX_CHARS,
    )
    .unwrap();
    assert_eq!(left.traces, vec!["tr-c"], "the newest carried over");
    let second = run(Some(2));
    assert_eq!(second.selected, 1);
    assert!(
        find_backlog(
            &env.receipts,
            &env.store,
            &env.root,
            &required,
            None,
            DEFAULT_CLAIMS_SOURCE_MAX_CHARS
        )
        .unwrap()
        .traces
        .is_empty()
    );
    assert_eq!(run(None).selected, 0, "a drained backlog selects nothing");
}

#[test]
fn a_run_writes_a_current_row_for_every_required_check_of_every_model() {
    let env = env();
    stage(&env.root, "tr-1", "a claim");
    succeed(&env.receipts, "tr-1", "2026-10-17T10:00:00Z");
    let judges = |_: &EvalOpts| -> Result<(Box<dyn DistillationJudge>, Box<dyn ClaimJudge>)> {
        Ok((
            Box::new(MockJudge::new(axes())),
            Box::new(MockClaimJudge::new(ClaimVerdict::Supported)),
        ))
    };
    let two = vec![MODEL.to_string(), "claude-sonnet-5".to_string()];
    let run = run_unscored(
        &env.receipts,
        &env.store,
        &env.cache,
        &EvalOpts::default(),
        &env.root,
        &two,
        None,
        &judges,
    )
    .unwrap();
    assert_eq!(run.reports.len(), 2);
    assert!(!run.all_errored());
    let rows = env.store.rows_for("tr-1").unwrap();
    assert!(
        rows.iter()
            .all(|r| r.run_kind == RUN_KIND_BASELINE && r.run_id.is_empty() && r.error.is_none())
    );
    for model in &two {
        for check in [CHECK_JUDGE_DISTILLATION, CHECK_JUDGE_CLAIMS] {
            assert!(
                rows.iter().any(|r| &r.judge_model == model && r.check_name == check),
                "missing {model} {check}"
            );
        }
    }
    assert_eq!(
        rows.iter().filter(|r| r.judge_model.is_empty()).count(),
        DETERMINISTIC_CHECKS.len(),
        "deterministic rows overwrite themselves across models"
    );
    assert!(rows.iter().any(|r| r.check_name == CHECK_NOTE_SIZE));
    let covered = env
        .store
        .is_covered(
            "tr-1",
            &current_hash(&env, "tr-1"),
            &required_checks(&two),
            DEFAULT_CLAIMS_SOURCE_MAX_CHARS,
        )
        .unwrap();
    assert!(covered);
}

struct FailingJudge;
impl DistillationJudge for FailingJudge {
    fn judge(&self, _: &str, _: &str, _: &str) -> Result<AxisScores> {
        eyre::bail!("vendor 503")
    }
}

#[test]
fn a_failing_judge_writes_error_rows_and_leaves_the_trace_owed() {
    let env = env();
    stage(&env.root, "tr-1", "a claim");
    succeed(&env.receipts, "tr-1", "2026-10-17T10:00:00Z");
    let judges = |_: &EvalOpts| -> Result<(Box<dyn DistillationJudge>, Box<dyn ClaimJudge>)> {
        Ok((
            Box::new(FailingJudge),
            Box::new(MockClaimJudge::new(ClaimVerdict::Supported)),
        ))
    };
    let run = run_unscored(
        &env.receipts,
        &env.store,
        &env.cache,
        &EvalOpts::default(),
        &env.root,
        &models(),
        None,
        &judges,
    )
    .unwrap();
    assert!(run.all_errored(), "the one trace errored on the one model");
    let rows = env.store.rows_for("tr-1").unwrap();
    let err = rows.iter().find(|r| r.check_name == CHECK_JUDGE_DISTILLATION).unwrap();
    assert_eq!(err.error.as_deref(), Some("vendor 503"));
    let backlog = find_backlog(
        &env.receipts,
        &env.store,
        &env.root,
        &required_checks(&models()),
        None,
        DEFAULT_CLAIMS_SOURCE_MAX_CHARS,
    )
    .unwrap();
    assert_eq!(backlog.traces, vec!["tr-1"], "the next run retries it");
}

#[test]
fn empty_judge_models_is_an_error_not_a_default() {
    let env = env();
    let judges = |_: &EvalOpts| -> Result<(Box<dyn DistillationJudge>, Box<dyn ClaimJudge>)> {
        unreachable!("no judge is built")
    };
    let err = run_unscored(
        &env.receipts,
        &env.store,
        &env.cache,
        &EvalOpts::default(),
        &env.root,
        &[],
        None,
        &judges,
    )
    .err()
    .expect("must fail")
    .to_string();
    assert!(err.contains("eval.judge-models"), "{err}");
}

#[test]
fn preflight_names_the_vendor_and_var_that_do_not_resolve_and_never_a_value() {
    let keys: vault::fabric::ApiKeys =
        serde_yaml::from_str("gemini: SB_EVAL_TEST_UNSET_GEMINI_VAR\nopenai: SB_EVAL_TEST_UNSET_OPENAI_VAR\n").unwrap();
    let err = preflight_keys(&keys).unwrap_err().to_string();
    assert!(err.contains("SB_EVAL_TEST_UNSET_GEMINI_VAR"), "{err}");
    assert!(err.contains("SB_EVAL_TEST_UNSET_OPENAI_VAR"), "{err}");
    assert!(preflight_keys(&vault::fabric::ApiKeys::default()).is_ok());
}

#[test]
fn preflight_passes_when_a_named_var_is_a_readable_file() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("key");
    std::fs::write(&file, "not-a-real-key\n").unwrap();
    let keys = vault::fabric::ApiKeys::default().with(vault::fabric::Vendor::Gemini, file.display().to_string());
    assert!(preflight_keys(&keys).is_ok());
}

#[test]
fn a_scratch_succeeded_receipt_is_never_owed_a_score() {
    let env = env();
    stage(&env.root, "tr-ingest", "a claim");
    succeed(&env.receipts, "tr-ingest", "2026-10-17T10:00:00Z");
    // A distill run that succeeded and is staged, so it would qualify as owed.
    stage(&env.root, "tr-scratch", "a claim");
    receipts::record_received_scratch(&env.receipts, "tr-scratch", Method::Cli, ReceiptKind::Url, "u").unwrap();
    receipts::mark_succeeded(&env.receipts, "tr-scratch", "n.md", false).unwrap();
    let backlog = find_backlog(
        &env.receipts,
        &env.store,
        &env.root,
        &required_checks(&models()),
        None,
        DEFAULT_CLAIMS_SOURCE_MAX_CHARS,
    )
    .unwrap();
    assert_eq!(backlog.traces, vec!["tr-ingest"]);
    assert_eq!(backlog.not_staged, 0, "the scratch row is not even counted");
}
