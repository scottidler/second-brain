#![allow(clippy::unwrap_used)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};

use super::*;
use crate::eval::scores::{DETERMINISTIC_CHECKS, DETERMINISTIC_RUBRIC_VERSION, NO_MODEL};
use crate::eval::{RUN_KIND_BASELINE, cache};
use crate::receipts;
use vault::distilled::{Claim, Distilled};
use vault::receipts::ReceiptKind;
use vault::schema::Method;

const MODEL: &str = "gemini-2.5-pro";
const TRACE: &str = "tr-failing";
const URL: &str = "https://example.com/failing";

fn models() -> Vec<String> {
    vec![MODEL.to_string()]
}

/// Stage `trace` as an article with one quoted claim.
fn stage(root: &Path, trace: &str) {
    let dir = root.join(trace);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("envelope.yml"), "kind: article-url\n").unwrap();
    let distilled = Distilled {
        summary: format!("summary of {trace}"),
        claims: vec![Claim {
            text: "the sky is green".to_string(),
            quote: Some("marmalade trombone".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    };
    std::fs::write(dir.join("distilled.yml"), serde_yaml::to_string(&distilled).unwrap()).unwrap();
    std::fs::write(dir.join("transcript.md"), format!("source text for {trace}")).unwrap();
}

fn succeed(conn: &Connection, trace: &str, url: &str) {
    receipts::record_received(conn, trace, Method::Telegram, ReceiptKind::Url, url).unwrap();
    receipts::mark_succeeded(conn, trace, "inbox/x.md", false).unwrap();
}

fn current_hash(root: &Path, trace: &str) -> String {
    judged_input(&load_staged_trace(root, trace).unwrap().unwrap()).content_hash
}

fn row(trace: &str, hash: &str, model: &str, check: &str, rubric: &str, metrics: Value) -> ScoreRow {
    ScoreRow {
        trace: trace.to_string(),
        run_kind: RUN_KIND_BASELINE.to_string(),
        run_id: String::new(),
        kind: "article".to_string(),
        judge_model: model.to_string(),
        check_name: check.to_string(),
        rubric_version: rubric.to_string(),
        content_hash: hash.to_string(),
        scored_at: "2026-10-17T00:00:00Z".to_string(),
        metrics_json: metrics.to_string(),
        error: None,
    }
}

fn passing_metrics(check: &str) -> Value {
    match check {
        CHECK_PLACEHOLDER_ARTIFACTS => serde_json::json!({ "reasons": [] }),
        CHECK_NOTE_SIZE => serde_json::json!({ "rendered_bytes": 100, "within_ceiling": true }),
        _ => serde_json::json!({ "applicable": false }),
    }
}

/// How the trace's current rows read.
#[derive(Clone, Copy)]
struct Scores {
    quote_grounding_fails: bool,
    judge_claims_fails: bool,
}

/// A current, error-free row for every required check of [`models`].
fn score(store: &ScoreStore, root: &Path, trace: &str, scores: Scores) {
    let hash = current_hash(root, trace);
    for check in DETERMINISTIC_CHECKS {
        let metrics = if check == CHECK_QUOTE_GROUNDING && scores.quote_grounding_fails {
            serde_json::json!({ "grounded": 0, "quoted": 1 })
        } else {
            passing_metrics(check)
        };
        store
            .put(&row(
                trace,
                &hash,
                NO_MODEL,
                check,
                DETERMINISTIC_RUBRIC_VERSION,
                metrics,
            ))
            .unwrap();
    }
    store
        .put(&row(
            trace,
            &hash,
            MODEL,
            CHECK_JUDGE_DISTILLATION,
            cache::RUBRIC_VERSION,
            serde_json::json!({ "claim_coverage": 3, "anchor_validity": 3, "summary_faithfulness": 3 }),
        ))
        .unwrap();
    let verdict = if scores.judge_claims_fails { "unsupported" } else { "supported" };
    store
        .put(&row(
            trace,
            &hash,
            MODEL,
            CHECK_JUDGE_CLAIMS,
            cache::CLAIMS_RUBRIC_VERSION,
            serde_json::json!({ "verdicts": [verdict] }),
        ))
        .unwrap();
}

/// A `judge_trust` row for `judge-claims` number-swap, its only applicable
/// class (the applicability matrix).
fn claims_trust(paired: f64, n: usize) -> TrustRow {
    TrustRow {
        judge_model: MODEL.to_string(),
        check_name: CHECK_JUDGE_CLAIMS.to_string(),
        planted_class: "number-swap".to_string(),
        rubric_version: cache::CLAIMS_RUBRIC_VERSION.to_string(),
        paired_catch_rate: paired,
        raw_catch_rate: 0.96,
        flag_rate: 0.38,
        wilson_lower: 0.42,
        sample_size: n,
        measured_at: "2026-10-17T13:58:31Z".to_string(),
    }
}

/// Counts its posts and queues each one as `rem-<n>`.
#[derive(Default, Clone)]
struct CountingPoster {
    posts: Arc<AtomicUsize>,
    bodies: Arc<std::sync::Mutex<Vec<Value>>>,
}

impl Poster for CountingPoster {
    async fn post(&self, body: &Value) -> std::result::Result<IngestResult, PostError> {
        let n = self.posts.fetch_add(1, Ordering::SeqCst) + 1;
        self.bodies.lock().unwrap().push(body.clone());
        Ok(IngestResult {
            status: IngestStatus::Queued,
            trace_id: Some(format!("rem-{n}")),
            ..Default::default()
        })
    }
}

/// Answers every post with the same error.
struct FailingPoster(PostError);

impl Poster for FailingPoster {
    async fn post(&self, _body: &Value) -> std::result::Result<IngestResult, PostError> {
        Err(self.0.clone())
    }
}

fn no_note(_trace: &str) -> Result<Option<String>> {
    Ok(None)
}

fn block_on<F: Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

struct Env {
    _tmp: tempfile::TempDir,
    root: std::path::PathBuf,
    receipts_path: std::path::PathBuf,
    cache_path: std::path::PathBuf,
}

/// One failing URL trace, staged, received and scored, on file DBs so several
/// connections (two concurrent runs) see the same state.
fn env(scores: Scores) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("stages");
    let receipts_path = tmp.path().join("receipts.db");
    let cache_path = tmp.path().join("eval-cache.db");
    stage(&root, TRACE);
    succeed(&receipts::open_at(&receipts_path).unwrap(), TRACE, URL);
    score(&ScoreStore::open(&cache_path).unwrap(), &root, TRACE, scores);
    Env {
        _tmp: tmp,
        root,
        receipts_path,
        cache_path,
    }
}

impl Env {
    fn plan(&self, conn: &Connection, trust_rows: &[TrustRow]) -> Plan {
        let store = ScoreStore::open(&self.cache_path).unwrap();
        plan(conn, &store, &self.root, &models(), trust_rows, &TrustConfig::default()).unwrap()
    }

    /// One `--remediate` run; `poster` None is the dry run.
    fn run<P: Poster>(&self, trust_rows: &[TrustRow], poster: Option<&P>) -> Report {
        let conn = receipts::open_at(&self.receipts_path).unwrap();
        let plan = self.plan(&conn, trust_rows);
        block_on(run(&conn, plan, poster, &no_note)).unwrap()
    }

    fn claims(&self) -> usize {
        let conn = receipts::open_at(&self.receipts_path).unwrap();
        conn.query_row("SELECT COUNT(*) FROM remediations", [], |r| r.get::<_, i64>(0))
            .unwrap() as usize
    }
}

const DETERMINISTIC_FAILURE: Scores = Scores {
    quote_grounding_fails: true,
    judge_claims_fails: false,
};
const UNTRUSTED_JUDGE_FAILURE_ONLY: Scores = Scores {
    quote_grounding_fails: false,
    judge_claims_fails: true,
};

#[test]
fn two_concurrent_remediate_runs_on_the_same_failing_source_post_exactly_once() {
    // Several rounds, each on fresh DBs: two runs plan, meet at a barrier, and
    // race to claim the one source.
    for round in 0..12 {
        let env = env(DETERMINISTIC_FAILURE);
        let poster = CountingPoster::default();
        let barrier = Arc::new(Barrier::new(2));
        let outcomes: Vec<Outcome> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    let (env, poster, barrier) = (&env, poster.clone(), Arc::clone(&barrier));
                    s.spawn(move || {
                        let conn = receipts::open_at(&env.receipts_path).unwrap();
                        let plan = env.plan(&conn, &[]);
                        barrier.wait();
                        let report = block_on(run(&conn, plan, Some(&poster), &no_note)).unwrap();
                        report.outcomes.into_iter().next().unwrap()
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert_eq!(poster.posts.load(Ordering::SeqCst), 1, "round {round}: {outcomes:?}");
        assert_eq!(
            outcomes.iter().filter(|o| matches!(o, Outcome::Posted { .. })).count(),
            1,
            "round {round}: {outcomes:?}"
        );
        assert!(
            outcomes.contains(&Outcome::AlreadyAttempted { prior: 1 }),
            "round {round}: {outcomes:?}"
        );
        assert_eq!(env.claims(), 1);
    }
}

#[test]
fn a_remediated_source_that_fails_again_is_never_posted_a_second_time() {
    let env = env(DETERMINISTIC_FAILURE);
    let poster = CountingPoster::default();
    let first = env.run(&[], Some(&poster));
    assert_eq!(
        first.outcomes,
        vec![Outcome::Posted {
            remediation_trace: "rem-1".to_string()
        }]
    );
    let conn = receipts::open_at(&env.receipts_path).unwrap();
    assert_eq!(remediation_trace_of(&conn, URL).unwrap().as_deref(), Some("rem-1"));
    // The remediation's own intake row (as the daemon writes it) does not
    // count as a second attempt beside the claim that posted it.
    receipts::record_received_with_lineage(
        &conn,
        "rem-1",
        Method::Telegram,
        ReceiptKind::Url,
        URL,
        Some(&receipts::Lineage {
            replay_of: TRACE.to_string(),
            origin: Some(ReplayOrigin::AutoRemediation),
        }),
    )
    .unwrap();
    let second = env.run(&[], Some(&poster));
    assert_eq!(second.outcomes, vec![Outcome::AlreadyAttempted { prior: 1 }]);
    assert_eq!(poster.posts.load(Ordering::SeqCst), 1);
}

#[test]
fn an_auto_remediation_intake_row_with_no_claim_still_counts_as_the_attempt() {
    // An attempt counts once its intake row exists, whatever happened after
    // (here: the claim table lost it).
    let env = env(DETERMINISTIC_FAILURE);
    let conn = receipts::open_at(&env.receipts_path).unwrap();
    receipts::record_received_with_lineage(
        &conn,
        "rem-elsewhere",
        Method::Telegram,
        ReceiptKind::Url,
        URL,
        Some(&receipts::Lineage {
            replay_of: TRACE.to_string(),
            origin: Some(ReplayOrigin::AutoRemediation),
        }),
    )
    .unwrap();
    // A plain replay with lineage but no auto-remediation origin is not one.
    receipts::record_received_with_lineage(
        &conn,
        "manual-replay",
        Method::Cli,
        ReceiptKind::Url,
        URL,
        Some(&receipts::Lineage {
            replay_of: TRACE.to_string(),
            origin: None,
        }),
    )
    .unwrap();
    let poster = CountingPoster::default();
    let report = env.run(&[], Some(&poster));
    assert_eq!(report.outcomes, vec![Outcome::AlreadyAttempted { prior: 1 }]);
    assert_eq!(poster.posts.load(Ordering::SeqCst), 0);
}

#[test]
fn a_failure_only_an_untrusted_judge_reports_triggers_nothing_and_names_judge_check_and_class() {
    let env = env(UNTRUSTED_JUDGE_FAILURE_ONLY);
    let poster = CountingPoster::default();
    // The measured Gemini judge-claims trust: paired catch 0.58 over 26 number swaps.
    let report = env.run(&[claims_trust(0.58, 26)], Some(&poster));
    assert_eq!(report.outcomes, vec![Outcome::Blocked]);
    assert_eq!(poster.posts.load(Ordering::SeqCst), 0);
    assert_eq!(env.claims(), 0, "a blocked failure claims nothing");
    let text = report.render();
    assert!(
        text.contains("gate: judge gemini-2.5-pro check judge-claims class number-swap untrusted"),
        "{text}"
    );
    assert!(text.contains("paired catch 0.58 < 0.80"), "{text}");
    assert!(text.contains("n 26 < min-samples 30"), "{text}");
    assert!(text.contains("blocked (untrusted judge only) 1"), "{text}");
}

#[test]
fn a_never_measured_judge_is_untrusted_too() {
    let env = env(UNTRUSTED_JUDGE_FAILURE_ONLY);
    let poster = CountingPoster::default();
    let report = env.run(&[], Some(&poster));
    assert_eq!(report.outcomes, vec![Outcome::Blocked]);
    assert!(report.render().contains("class number-swap untrusted (never measured)"));
    assert_eq!(poster.posts.load(Ordering::SeqCst), 0);
}

#[test]
fn a_trusted_judge_failure_triggers_a_remediation() {
    let env = env(UNTRUSTED_JUDGE_FAILURE_ONLY);
    let poster = CountingPoster::default();
    let report = env.run(&[claims_trust(0.80, 30)], Some(&poster));
    assert!(matches!(report.outcomes[..], [Outcome::Posted { .. }]), "{report:?}");
    assert_eq!(poster.posts.load(Ordering::SeqCst), 1);
}

#[test]
fn a_deterministic_check_failure_triggers_a_remediation_even_when_no_judge_check_is_trusted() {
    // Quote grounding fails AND the judge fails untrusted; no judge_trust row
    // at all, so no judge check is trusted.
    let env = env(Scores {
        quote_grounding_fails: true,
        judge_claims_fails: true,
    });
    let poster = CountingPoster::default();
    let report = env.run(&[], Some(&poster));
    assert!(matches!(report.outcomes[..], [Outcome::Posted { .. }]), "{report:?}");
    assert_eq!(poster.posts.load(Ordering::SeqCst), 1);
    let candidate = &report.plan.candidates[0];
    let trigger = candidate.trigger().unwrap();
    assert_eq!(
        (trigger.check.as_str(), trigger.judge_model.as_str()),
        (CHECK_QUOTE_GROUNDING, "")
    );
    // The frozen "before" is the trigger's eval_scores key plus a copy of its
    // metrics, so a later overwrite of that key cannot rewrite it.
    let conn = receipts::open_at(&env.receipts_path).unwrap();
    let (check, hash, metrics): (String, String, Option<String>) = conn
        .query_row(
            "SELECT trigger_check, trigger_content_hash, trigger_metrics_json FROM remediations WHERE source_url = ?1",
            params![URL],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(check, CHECK_QUOTE_GROUNDING);
    assert_eq!(hash, current_hash(&env.root, TRACE));
    assert_eq!(
        metrics.as_deref(),
        Some(serde_json::json!({ "grounded": 0, "quoted": 1 }).to_string().as_str())
    );
    // The body carries the lineage and the original method.
    let body = poster.bodies.lock().unwrap()[0].clone();
    assert_eq!(body["replay-of"], TRACE);
    assert_eq!(body["replay-origin"], "auto-remediation");
    assert_eq!(body["method"], "telegram");
    assert_eq!(body["force"], true);
    assert_eq!(body["url"], URL);
}

#[test]
fn the_dry_run_shows_the_body_and_writes_nothing() {
    let env = env(DETERMINISTIC_FAILURE);
    let report = env.run::<CountingPoster>(&[], None);
    let [Outcome::WouldPost(body)] = &report.outcomes[..] else {
        panic!("{report:?}");
    };
    assert_eq!(body["replay-of"], TRACE);
    assert_eq!(env.claims(), 0);
    let text = report.render();
    assert!(text.contains("remediation (dry run; --apply posts)"), "{text}");
    assert!(
        text.contains("FAIL quote-grounding (deterministic, ungated): 0 of 1 quotes grounded"),
        "{text}"
    );
}

#[test]
fn a_post_the_daemon_never_got_releases_the_claim_and_an_unknown_one_keeps_it() {
    let env = env(DETERMINISTIC_FAILURE);
    let refused = FailingPoster(PostError::NotDelivered("connection refused".to_string()));
    let report = env.run(&[], Some(&refused));
    assert!(matches!(report.outcomes[..], [Outcome::NotDelivered(_)]));
    assert!(report.post_failed());
    assert_eq!(env.claims(), 0, "no intake row can exist, so the source may be retried");

    let lost = FailingPoster(PostError::Unknown("timed out".to_string()));
    let report = env.run(&[], Some(&lost));
    assert!(matches!(report.outcomes[..], [Outcome::Unknown(_)]));
    assert_eq!(env.claims(), 1, "intake may have run: the attempt stands");
    let poster = CountingPoster::default();
    assert_eq!(
        env.run(&[], Some(&poster)).outcomes,
        vec![Outcome::AlreadyAttempted { prior: 1 }]
    );
}

#[test]
fn error_rows_other_judges_and_stale_hashes_never_trigger() {
    let env = env(Scores {
        quote_grounding_fails: false,
        judge_claims_fails: false,
    });
    let store = ScoreStore::open(&env.cache_path).unwrap();
    let hash = current_hash(&env.root, TRACE);
    let failing_claims = serde_json::json!({ "verdicts": ["unsupported"] });
    // An error row at the current hash for a judge outside the required set.
    store
        .put(&ScoreRow {
            error: Some("404".to_string()),
            ..row(
                TRACE,
                &hash,
                "other-model",
                CHECK_JUDGE_CLAIMS,
                cache::CLAIMS_RUBRIC_VERSION,
                failing_claims.clone(),
            )
        })
        .unwrap();
    // A failing row from a judge outside the required set.
    store
        .put(&row(
            TRACE,
            &hash,
            "claude-sonnet-5",
            CHECK_JUDGE_CLAIMS,
            cache::CLAIMS_RUBRIC_VERSION,
            failing_claims.clone(),
        ))
        .unwrap();
    // A failing row at an older distill hash.
    store
        .put(&row(
            TRACE,
            "0ldhash",
            NO_MODEL,
            CHECK_QUOTE_GROUNDING,
            DETERMINISTIC_RUBRIC_VERSION,
            serde_json::json!({ "grounded": 0, "quoted": 2 }),
        ))
        .unwrap();
    // A failing tool row (not a required check).
    store
        .put(&row(
            TRACE,
            &hash,
            "roberta-large",
            "minicheck",
            cache::RUBRIC_VERSION,
            serde_json::json!({}),
        ))
        .unwrap();
    let conn = receipts::open_at(&env.receipts_path).unwrap();
    assert!(env.plan(&conn, &[]).candidates.is_empty());
}

#[test]
fn an_unreadable_current_row_fails_the_run_instead_of_passing() {
    let env = env(DETERMINISTIC_FAILURE);
    let store = ScoreStore::open(&env.cache_path).unwrap();
    let hash = current_hash(&env.root, TRACE);
    store
        .put(&row(
            TRACE,
            &hash,
            NO_MODEL,
            CHECK_NOTE_SIZE,
            DETERMINISTIC_RUBRIC_VERSION,
            serde_json::json!({ "bytes": 1 }),
        ))
        .unwrap();
    let conn = receipts::open_at(&env.receipts_path).unwrap();
    let err = plan(&conn, &store, &env.root, &models(), &[], &TrustConfig::default()).unwrap_err();
    assert!(format!("{err:#}").contains("note-size"), "{err:#}");
}

#[test]
fn a_non_url_receipt_is_listed_not_remediable() {
    let env = env(DETERMINISTIC_FAILURE);
    let conn = receipts::open_at(&env.receipts_path).unwrap();
    conn.execute("UPDATE receipts SET kind = 'text' WHERE trace_id = ?1", params![TRACE])
        .unwrap();
    let poster = CountingPoster::default();
    let report = env.run(&[], Some(&poster));
    assert!(matches!(report.outcomes[..], [Outcome::NotRemediable(_)]));
    assert_eq!(poster.posts.load(Ordering::SeqCst), 0);
}

#[test]
fn each_deterministic_check_fails_as_the_report_reads_it() {
    let r = |check: &str, metrics: Value| {
        row_failure(&row("t", "h", NO_MODEL, check, DETERMINISTIC_RUBRIC_VERSION, metrics)).unwrap()
    };
    assert!(r(CHECK_QUOTE_GROUNDING, serde_json::json!({ "grounded": 1, "quoted": 1 })).is_none());
    assert!(r(CHECK_QUOTE_GROUNDING, serde_json::json!({ "applicable": false })).is_none());
    let tools =
        |ng, n, cg, c| serde_json::json!({ "names_grounded": ng, "names": n, "commands_grounded": cg, "commands": c });
    assert!(r(CHECK_TOOLS_GROUNDING, tools(2, 2, 1, 1)).is_none());
    assert!(r(CHECK_TOOLS_GROUNDING, tools(1, 2, 1, 1)).is_some());
    assert!(r(CHECK_PLACEHOLDER_ARTIFACTS, serde_json::json!({ "reasons": [] })).is_none());
    assert!(
        r(
            CHECK_PLACEHOLDER_ARTIFACTS,
            serde_json::json!({ "reasons": ["summary: TBD"] })
        )
        .is_some()
    );
    assert!(r(CHECK_LISTICLE_SURVIVAL, serde_json::json!({ "score": 1.0 })).is_none());
    assert!(r(CHECK_LISTICLE_SURVIVAL, serde_json::json!({ "score": 0.6 })).is_some());
    assert!(
        r(
            CHECK_NOTE_SIZE,
            serde_json::json!({ "rendered_bytes": 9, "within_ceiling": false })
        )
        .is_some()
    );
    assert!(row_failure(&row("t", "h", NO_MODEL, "no-such-check", "v1", serde_json::json!({}))).is_err());
}

#[test]
fn a_definitive_refusal_is_not_delivered_and_a_5xx_is_unknown() {
    let status = |status| DaemonError::Status {
        addr: "a".to_string(),
        path: "/ingest".to_string(),
        status,
        body: String::new(),
    };
    assert!(matches!(classify(&status(422)), PostError::NotDelivered(_)));
    assert!(matches!(classify(&status(502)), PostError::Unknown(_)));
    assert!(matches!(
        classify(&DaemonError::Unauthorized {
            addr: "a".to_string(),
            path: "/ingest".to_string()
        }),
        PostError::NotDelivered(_)
    ));
}

/// Drives the real `/ingest` route in-process.
struct RouterPoster(axum::Router);

impl Poster for RouterPoster {
    async fn post(&self, body: &Value) -> std::result::Result<IngestResult, PostError> {
        use tower::ServiceExt;
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/ingest")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap();
        let resp = self.0.clone().oneshot(req).await.unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).map_err(|e| PostError::Unknown(e.to_string()))
    }
}

#[tokio::test]
async fn an_applied_remediation_lands_lineage_on_the_daemons_receipts_row() {
    let data_home = tempfile::tempdir().unwrap();
    crate::harvest::with_xdg_data_home(data_home.path(), || async {
        let vault = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(vault.path().join(".obsidian")).unwrap();
        let mut config = crate::config::Config::default();
        config.vault.root_path = Some(vault.path().to_string_lossy().to_string());
        let app = crate::build_router(crate::AppState {
            config: std::sync::Arc::new(config),
            telegram: None,
            desktop: None,
            version: "0.0.0-test".to_string(),
            auth_token: None,
        });

        let stages = data_home.path().join("stages");
        stage(&stages, TRACE);
        let conn = receipts::open_default().unwrap();
        succeed(&conn, TRACE, URL);
        let store = ScoreStore::open_memory().unwrap();
        score(&store, &stages, TRACE, DETERMINISTIC_FAILURE);
        let plan = plan(&conn, &store, &stages, &models(), &[], &TrustConfig::default()).unwrap();

        let report = run(&conn, plan, Some(&RouterPoster(app)), &no_note).await.unwrap();
        let [Outcome::Posted { remediation_trace }] = &report.outcomes[..] else {
            panic!("{report:?}");
        };
        let row = receipts::get(&conn, remediation_trace)
            .unwrap()
            .expect("the daemon's intake row");
        assert_eq!(row.replay_of.as_deref(), Some(TRACE));
        assert_eq!(row.replay_origin.as_deref(), Some("auto-remediation"));
        assert_eq!(row.raw_input, URL);
        assert_eq!(row.method, "telegram");
        let lineaged: i64 = conn
            .query_row("SELECT COUNT(*) FROM receipts WHERE replay_of IS NOT NULL", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(lineaged, 1);
        // The claim names the trace the daemon queued, and a second run is refused.
        assert_eq!(
            remediation_trace_of(&conn, URL).unwrap().as_deref(),
            Some(remediation_trace.as_str())
        );
        assert_eq!(prior_attempts(&conn, URL).unwrap(), 1);
    })
    .await;
}

/// A scratch run that succeeded, is staged and scores as failing would be a
/// candidate (and a POST to `/ingest`) if the plan read it; it is not.
#[test]
fn a_scratch_succeeded_failing_trace_is_never_a_candidate() {
    let env = env(DETERMINISTIC_FAILURE);
    let conn = receipts::open_at(&env.receipts_path).unwrap();
    let baseline = env.plan(&conn, &[]);
    assert_eq!(baseline.candidates.len(), 1);

    let scratch_trace = "tr-scratch";
    stage(&env.root, scratch_trace);
    receipts::record_received_scratch(
        &conn,
        scratch_trace,
        Method::Cli,
        ReceiptKind::Url,
        "https://example.com/s",
    )
    .unwrap();
    receipts::mark_succeeded(&conn, scratch_trace, "n.md", false).unwrap();
    score(
        &ScoreStore::open(&env.cache_path).unwrap(),
        &env.root,
        scratch_trace,
        DETERMINISTIC_FAILURE,
    );

    let plan = env.plan(&conn, &[]);
    assert_eq!(plan.candidates.len(), 1, "{plan:?}");
    assert_eq!(plan.candidates[0].trace, TRACE);
    assert_eq!(
        plan.no_receipt,
        baseline.no_receipt + 1,
        "the scratch trace reads as no receipt"
    );
}
