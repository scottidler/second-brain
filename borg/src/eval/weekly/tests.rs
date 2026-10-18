#![allow(clippy::unwrap_used)]
use crate::config::DEFAULT_CLAIMS_SOURCE_MAX_CHARS;

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::eval::scores::{
    CHECK_PLACEHOLDER_ARTIFACTS, CHECK_QUOTE_GROUNDING, DETERMINISTIC_CHECKS, DETERMINISTIC_RUBRIC_VERSION, NO_MODEL,
    ScoreRow,
};
use crate::eval::{CHECK_JUDGE_DISTILLATION, RUN_KIND_BASELINE, cache, judged_input};
use crate::receipts::{self, Lineage};
use crate::types::ReplayOrigin;
use cluster::{RawCategory, UNCLUSTERED, assemble, cluster_failures, parse_reply, prompt_input};
use vault::distilled::{Claim, Distilled};
use vault::receipts::ReceiptKind;
use vault::schema::Method;

const MODEL: &str = "gemini-2.5-pro";

fn date() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 17).unwrap()
}

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

fn hash(root: &Path, trace: &str) -> String {
    judged_input(&load_staged_trace(root, trace).unwrap().unwrap()).content_hash
}

fn row(trace: &str, hash: &str, model: &str, check: &str, rubric: &str, metrics: serde_json::Value) -> ScoreRow {
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

/// How a scored trace's rows read.
#[derive(Clone, Copy, PartialEq)]
enum Scores {
    AllPass,
    QuoteGroundingFails,
    JudgeClaimsFails,
    EveryRowErrored,
    /// Every check passes, but `judge-claims` is not judgeable (source over
    /// the claims cap).
    ClaimsNotJudgeable,
}

fn score(store: &ScoreStore, root: &Path, trace: &str, scores: Scores) {
    let h = hash(root, trace);
    let put = |r: ScoreRow| {
        let r = if scores == Scores::EveryRowErrored {
            ScoreRow {
                error: Some("judge call failed".to_string()),
                ..r
            }
        } else {
            r
        };
        store.put(&r).unwrap();
    };
    for check in DETERMINISTIC_CHECKS {
        let metrics = match check {
            CHECK_QUOTE_GROUNDING if scores == Scores::QuoteGroundingFails => {
                serde_json::json!({ "grounded": 0, "quoted": 1 })
            }
            CHECK_PLACEHOLDER_ARTIFACTS => serde_json::json!({ "reasons": [] }),
            crate::eval::scores::CHECK_NOTE_SIZE => {
                serde_json::json!({ "rendered_bytes": 100, "within_ceiling": true })
            }
            _ => serde_json::json!({ "applicable": false }),
        };
        put(row(trace, &h, NO_MODEL, check, DETERMINISTIC_RUBRIC_VERSION, metrics));
    }
    put(row(
        trace,
        &h,
        MODEL,
        CHECK_JUDGE_DISTILLATION,
        cache::RUBRIC_VERSION,
        serde_json::json!({ "claim_coverage": 3, "anchor_validity": 3, "summary_faithfulness": 3 }),
    ));
    let verdict = if scores == Scores::JudgeClaimsFails { "unsupported" } else { "supported" };
    let claims = if scores == Scores::ClaimsNotJudgeable {
        serde_json::json!({ "applicable": false, "reason": "source-over-cap", "source_chars": 500000, "cap": 400000 })
    } else {
        serde_json::json!({ "verdicts": [verdict] })
    };
    put(row(
        trace,
        &h,
        MODEL,
        CHECK_JUDGE_CLAIMS,
        cache::CLAIMS_RUBRIC_VERSION,
        claims,
    ));
}

fn claims_trust(paired: f64, n: usize, class: &str) -> TrustRow {
    TrustRow {
        judge_model: MODEL.to_string(),
        check_name: CHECK_JUDGE_CLAIMS.to_string(),
        planted_class: class.to_string(),
        rubric_version: cache::CLAIMS_RUBRIC_VERSION.to_string(),
        paired_catch_rate: paired,
        raw_catch_rate: 0.96,
        flag_rate: 0.38,
        wilson_lower: 0.42,
        sample_size: n,
        measured_at: "2026-10-17T13:58:31Z".to_string(),
    }
}

struct Env {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    reports: PathBuf,
    receipts: Connection,
    store: ScoreStore,
    eval: EvalConfig,
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("stages");
        let reports = tmp.path().join("eval-reports");
        let receipts = receipts::open_at(&tmp.path().join("receipts.db")).unwrap();
        let eval = EvalConfig {
            judge_models: vec![MODEL.to_string()],
            ..EvalConfig::default()
        };
        Self {
            _tmp: tmp,
            root,
            reports,
            receipts,
            store: ScoreStore::open_memory().unwrap(),
            eval,
        }
    }

    /// Stage, receive and score one URL trace.
    fn trace(&self, trace: &str, scores: Scores) {
        stage(&self.root, trace);
        succeed(&self.receipts, trace, &format!("https://example.com/{trace}"));
        score(&self.store, &self.root, trace, scores);
    }

    fn inputs<'a>(&'a self, trust_rows: &'a [TrustRow]) -> Inputs<'a> {
        Inputs {
            receipts: &self.receipts,
            store: &self.store,
            staged_root: &self.root,
            eval: &self.eval,
            trust_rows,
            reports_dir: &self.reports,
        }
    }

    fn claim(&self, source: &str, original: &str, remediation: Option<&str>, post_error: Option<&str>) {
        let h = hash(&self.root, original);
        self.receipts
            .execute(
                "INSERT INTO remediations (source_url, original_trace, claimed_at, remediation_trace,
                    trigger_judge_model, trigger_check, trigger_rubric, trigger_content_hash, post_error,
                    trigger_metrics_json)
                 VALUES (?1, ?2, '2026-10-16T00:00:00Z', ?3, '', ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    source,
                    original,
                    remediation,
                    CHECK_QUOTE_GROUNDING,
                    DETERMINISTIC_RUBRIC_VERSION,
                    h,
                    post_error,
                    serde_json::json!({ "grounded": 0, "quoted": 1 }).to_string()
                ],
            )
            .unwrap();
    }

    /// The remediation's intake row, carrying lineage as the daemon writes it.
    fn remediation_receipt(&self, trace: &str, original: &str, source: &str) {
        receipts::record_received_with_lineage(
            &self.receipts,
            trace,
            Method::Telegram,
            ReceiptKind::Url,
            source,
            Some(&Lineage {
                replay_of: original.to_string(),
                origin: Some(ReplayOrigin::AutoRemediation),
            }),
        )
        .unwrap();
    }
}

/// Answers every call with one canned reply, recording what it was asked.
struct Canned {
    reply: String,
    calls: AtomicUsize,
    inputs: Mutex<Vec<String>>,
}

impl Canned {
    fn new(reply: &str) -> Self {
        Self {
            reply: reply.to_string(),
            calls: AtomicUsize::new(0),
            inputs: Mutex::new(Vec::new()),
        }
    }
}

impl Clusterer for Canned {
    fn cluster(&self, input: &str) -> Result<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.inputs.lock().unwrap().push(input.to_string());
        Ok(self.reply.clone())
    }
}

fn failures(ids: &[&str]) -> Vec<TraceFailures> {
    ids.iter()
        .map(|t| TraceFailures {
            trace: (*t).to_string(),
            kind: "article".to_string(),
            entries: vec![format!("quote-grounding: 0 of 1 quotes grounded for {t}")],
        })
        .collect()
}

fn raw(name: &str, traces: &[&str]) -> RawCategory {
    RawCategory {
        name: name.to_string(),
        description: format!("about {name}"),
        traces: traces.iter().map(|t| (*t).to_string()).collect(),
    }
}

// ---- clustering ----

#[test]
fn reply_parses_through_a_fence_and_a_preamble() {
    let reply = "Here you go:\n```json\n{\"categories\":[{\"name\":\"invented numbers\",\"description\":\"d\",\"traces\":[\"a\",\"b\"]}]}\n```";
    let cats = parse_reply(reply).unwrap();
    assert_eq!(cats.len(), 1);
    assert_eq!(cats[0].name, "invented numbers");
    assert_eq!(cats[0].traces, ["a", "b"]);
}

#[test]
fn a_reply_without_the_expected_object_is_an_error_not_an_empty_clustering() {
    let none = parse_reply("I could not find any categories").unwrap_err().to_string();
    assert!(none.contains("no JSON object"), "{none}");
    let wrong = parse_reply("{\"groups\":[]}").unwrap_err().to_string();
    assert!(wrong.contains("categories"), "{wrong}");
}

#[test]
fn counts_come_from_validated_trace_lists_not_from_the_model() {
    let input = failures(&["t1", "t2", "t3", "t4"]);
    // t2 listed twice, "ghost" invented: the count is 2 (t1, t2), not 3.
    let result = assemble(vec![raw("numbers", &["t1", "t2", "t2", "ghost"])], &input);
    let numbers = result.clusters.iter().find(|c| c.name == "numbers").unwrap();
    assert_eq!(numbers.traces, ["t1", "t2"]);
    assert_eq!(result.unknown_ids, 1);
}

#[test]
fn a_failing_trace_the_model_left_out_lands_in_unclustered() {
    let input = failures(&["t1", "t2", "t3"]);
    let result = assemble(vec![raw("numbers", &["t1"])], &input);
    let left = result.clusters.iter().find(|c| c.name == UNCLUSTERED).unwrap();
    assert_eq!(left.traces, ["t2", "t3"]);
    let total: usize = result.clusters.iter().map(|c| c.traces.len()).sum();
    assert_eq!(total, 3, "every failing trace is in exactly one category here");
}

#[test]
fn categories_sort_largest_first_and_an_all_unknown_category_is_dropped() {
    let input = failures(&["t1", "t2", "t3"]);
    let result = assemble(
        vec![
            raw("small", &["t1"]),
            raw("big", &["t2", "t3"]),
            raw("ghosts", &["x", "y"]),
        ],
        &input,
    );
    let names: Vec<&str> = result.clusters.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["big", "small"]);
    assert_eq!(result.unknown_ids, 2);
}

#[test]
fn examples_are_the_first_three_traces() {
    let input = failures(&["t1", "t2", "t3", "t4", "t5"]);
    let result = assemble(vec![raw("all", &["t1", "t2", "t3", "t4", "t5"])], &input);
    assert_eq!(result.clusters[0].traces.len(), 5);
    assert_eq!(result.clusters[0].examples(), ["t1", "t2", "t3"]);
}

#[test]
fn clustering_makes_exactly_one_call_and_none_when_nothing_failed() {
    let clusterer = Canned::new("{\"categories\":[{\"name\":\"n\",\"description\":\"d\",\"traces\":[\"t1\",\"t2\"]}]}");
    let result = cluster_failures(&clusterer, &failures(&["t1", "t2"])).unwrap();
    assert_eq!(clusterer.calls.load(Ordering::SeqCst), 1);
    assert_eq!(result.clusters[0].traces.len(), 2);

    let idle = Canned::new("unused");
    let empty = cluster_failures(&idle, &[]).unwrap();
    assert!(empty.clusters.is_empty());
    assert_eq!(idle.calls.load(Ordering::SeqCst), 0, "no failures, no model call");
}

#[test]
fn the_prompt_names_every_trace_and_its_failures() {
    let text = prompt_input(&failures(&["t1", "t2"]));
    for needle in [
        "trace: t1",
        "trace: t2",
        "quote-grounding: 0 of 1 quotes grounded for t2",
    ] {
        assert!(text.contains(needle), "{needle} missing from:\n{text}");
    }
    let long = cluster::claim_excerpt(&"word ".repeat(100));
    assert!(long.ends_with("..."), "{long}");
    assert!(long.chars().count() <= 163);
}

// ---- the report ----

#[test]
fn a_report_has_a_category_with_its_count_and_example_traces() {
    let env = Env::new();
    for t in ["tr-a", "tr-b", "tr-c", "tr-d"] {
        env.trace(t, Scores::QuoteGroundingFails);
    }
    env.trace("tr-ok", Scores::AllPass);
    let clusterer = Canned::new(
        "{\"categories\":[{\"name\":\"Fabricated quotes\",\"description\":\"A quote the source never says.\",\"traces\":[\"tr-a\",\"tr-b\",\"tr-c\",\"tr-d\"]}]}",
    );
    let report = build(&env.inputs(&[]), &clusterer, MODEL, date()).unwrap();
    let text = report.render();

    assert!(text.starts_with("# Eval report 2026-10-17"), "{text}");
    assert!(text.contains("### 1. Fabricated quotes (4 traces)"), "{text}");
    assert!(
        text.contains("Examples: tr-a, tr-b, tr-c\n"),
        "three examples, not four:\n{text}"
    );
    assert!(!text.contains("Examples: tr-a, tr-b, tr-c, tr-d"), "{text}");
    assert!(text.contains("- scored traces: 5"), "{text}");
    assert!(
        text.contains("- failing traces: 4 (4 actionable, 0 flagged only by untrusted judge checks)"),
        "{text}"
    );
    assert_eq!(
        clusterer.calls.load(Ordering::SeqCst),
        1,
        "one call for the whole report"
    );
    let sent = clusterer.inputs.lock().unwrap()[0].clone();
    assert!(sent.contains("trace: tr-d") && !sent.contains("tr-ok"), "{sent}");
}

#[test]
fn untrusted_only_failures_are_counted_separately_and_carry_the_rejected_claim_text() {
    let env = Env::new();
    env.trace("tr-judge", Scores::JudgeClaimsFails);
    let clusterer = Canned::new("{\"categories\":[]}");
    let report = build(&env.inputs(&[]), &clusterer, MODEL, date()).unwrap();
    let text = report.render();
    assert!(
        text.contains("- failing traces: 1 (0 actionable, 1 flagged only by untrusted judge checks)"),
        "{text}"
    );
    let sent = clusterer.inputs.lock().unwrap()[0].clone();
    assert!(sent.contains("unsupported claim: the sky is green"), "{sent}");
    assert!(sent.contains("[untrusted judge]"), "{sent}");
    assert!(
        text.contains(UNCLUSTERED),
        "an empty model reply must not drop the failure:\n{text}"
    );
}

#[test]
fn the_clustering_call_is_capped_at_max_failures_and_the_report_says_so() {
    let mut env = Env::new();
    env.eval.report.max_failures = 2;
    for t in ["tr-1", "tr-2", "tr-3"] {
        env.trace(t, Scores::QuoteGroundingFails);
    }
    let clusterer = Canned::new("{\"categories\":[]}");
    let report = build(&env.inputs(&[]), &clusterer, MODEL, date()).unwrap();
    assert_eq!(report.left_out, 1);
    let sent = clusterer.inputs.lock().unwrap()[0].clone();
    assert!(
        sent.contains("trace: tr-1") && sent.contains("trace: tr-2") && !sent.contains("tr-3"),
        "{sent}"
    );
    assert!(
        report
            .render()
            .contains("the clustering call was shown 2 failing traces; 1 more were not")
    );
}

#[test]
fn a_failing_clustering_call_fails_the_build() {
    struct Down;
    impl Clusterer for Down {
        fn cluster(&self, _input: &str) -> Result<String> {
            eyre::bail!("fabric is down")
        }
    }
    let env = Env::new();
    env.trace("tr-a", Scores::QuoteGroundingFails);
    let err = build(&env.inputs(&[]), &Down, MODEL, date()).unwrap_err();
    assert!(format!("{err:#}").contains("fabric is down"), "{err:#}");
}

#[test]
fn no_failures_renders_an_honest_empty_report_without_a_model_call() {
    let env = Env::new();
    env.trace("tr-ok", Scores::AllPass);
    let clusterer = Canned::new("unused");
    let report = build(&env.inputs(&[]), &clusterer, MODEL, date()).unwrap();
    assert_eq!(clusterer.calls.load(Ordering::SeqCst), 0);
    let text = report.render();
    assert!(text.contains("No failing traces this week."), "{text}");
}

// ---- trust states ----

#[test]
fn every_judge_check_prints_its_trust_state() {
    let env = Env::new();
    env.trace("tr-ok", Scores::AllPass);
    // judge-claims has one applicable class (number-swap): measured and clearing
    // the floor makes it trusted. judge-distillation was never measured.
    let trust = [claims_trust(0.90, 30, "number-swap")];
    let report = build(&env.inputs(&trust), &Canned::new("unused"), MODEL, date()).unwrap();
    let text = report.render();
    let claims = text.lines().find(|l| l.contains("judge-claims")).unwrap();
    assert!(claims.contains("| trusted |"), "{claims}");
    let distillation = text.lines().find(|l| l.contains("| judge-distillation |")).unwrap();
    assert!(distillation.contains("UNTRUSTED"), "{distillation}");
    assert!(distillation.contains("number-swap: never measured"), "{distillation}");
    assert!(
        text.contains(&format!("| {MODEL} | judge-claims |")),
        "each row names its judge model:\n{text}"
    );
}

#[test]
fn a_measured_but_weak_check_says_why_it_is_untrusted() {
    let env = Env::new();
    env.trace("tr-ok", Scores::AllPass);
    let trust = [claims_trust(0.58, 30, "number-swap")];
    let text = build(&env.inputs(&trust), &Canned::new("unused"), MODEL, date())
        .unwrap()
        .render();
    let claims = text.lines().find(|l| l.contains("| judge-claims |")).unwrap();
    assert!(
        claims.contains("UNTRUSTED") && claims.contains("paired catch 0.58 < 0.80"),
        "{claims}"
    );
}

// ---- Remediated ----

fn remediated(env: &Env) -> Vec<Remediation> {
    let required = required_checks(&env.eval.judge_models);
    remediated::collect(
        &env.receipts,
        &env.store,
        &env.root,
        &required,
        DEFAULT_CLAIMS_SOURCE_MAX_CHARS,
    )
    .unwrap()
}

#[test]
fn an_empty_remediated_section_says_so() {
    let env = Env::new();
    env.trace("tr-ok", Scores::AllPass);
    let text = build(&env.inputs(&[]), &Canned::new("unused"), MODEL, date())
        .unwrap()
        .render();
    assert!(
        text.contains("## Remediated\n\nNo source has been remediated."),
        "{text}"
    );
}

/// An original that failed, claimed and remediated into `rem-1`.
fn remediated_env(scores_of_remediation: Option<Scores>) -> Env {
    let env = Env::new();
    env.trace("orig", Scores::QuoteGroundingFails);
    env.claim("https://example.com/orig", "orig", Some("rem-1"), None);
    env.remediation_receipt("rem-1", "orig", "https://example.com/orig");
    if let Some(scores) = scores_of_remediation {
        stage(&env.root, "rem-1");
        receipts::mark_succeeded(&env.receipts, "rem-1", "inbox/y.md", false).unwrap();
        score(&env.store, &env.root, "rem-1", scores);
    }
    env
}

#[test]
fn outcome_pending_while_the_remediation_receipt_is_received() {
    let env = remediated_env(None);
    let r = &remediated(&env)[0];
    assert_eq!(r.outcome, Outcome::Pending);
    assert_eq!(r.remediation_trace.as_deref(), Some("rem-1"));
}

#[test]
fn outcome_attempt_failed_when_the_remediation_receipt_failed() {
    let env = remediated_env(None);
    receipts::mark_failed(
        &env.receipts,
        "rem-1",
        vault::receipts::FailureStage::FetchFailed,
        "dead url",
    )
    .unwrap();
    let r = &remediated(&env)[0];
    assert_eq!(r.outcome, Outcome::AttemptFailed);
    assert!(r.after.contains("dead url"), "{}", r.after);
}

#[test]
fn outcome_unscored_when_the_ingest_succeeded_and_nothing_scored_it() {
    let env = Env::new();
    env.trace("orig", Scores::QuoteGroundingFails);
    env.claim("https://example.com/orig", "orig", Some("rem-1"), None);
    env.remediation_receipt("rem-1", "orig", "https://example.com/orig");
    stage(&env.root, "rem-1");
    receipts::mark_succeeded(&env.receipts, "rem-1", "inbox/y.md", false).unwrap();
    assert_eq!(remediated(&env)[0].outcome, Outcome::Unscored);
}

#[test]
fn outcome_eval_errored_when_scoring_left_only_error_rows() {
    let env = remediated_env(Some(Scores::EveryRowErrored));
    assert_eq!(remediated(&env)[0].outcome, Outcome::EvalErrored);
}

#[test]
fn outcome_passed_when_the_remediated_trace_passes_every_required_check() {
    let env = remediated_env(Some(Scores::AllPass));
    let r = &remediated(&env)[0];
    assert_eq!(r.outcome, Outcome::Passed);
    assert!(r.after.contains("all 7 required checks pass"), "{}", r.after);
    assert!(
        r.before.contains("quote-grounding") && r.before.contains("0 of 1 quotes grounded"),
        "{}",
        r.before
    );
}

/// The "before" is the claim's frozen copy: overwriting the trigger's own
/// `eval_scores` key (what `--inject --rebuild-cache` does to an unmutated
/// original) leaves it unchanged.
#[test]
fn the_before_score_survives_an_overwrite_of_the_trigger_row() {
    let env = remediated_env(Some(Scores::AllPass));
    let h = hash(&env.root, "orig");
    let mut rescored = env
        .store
        .rows_for("orig")
        .unwrap()
        .into_iter()
        .find(|r| r.check_name == CHECK_QUOTE_GROUNDING && r.run_kind == RUN_KIND_BASELINE && r.content_hash == h)
        .unwrap();
    rescored.metrics_json = serde_json::json!({ "grounded": 1, "quoted": 1 }).to_string();
    rescored.scored_at = "2026-10-18T00:00:00Z".to_string();
    env.store.put(&rescored).unwrap();
    let live = env
        .store
        .rows_for("orig")
        .unwrap()
        .into_iter()
        .find(|r| r.check_name == CHECK_QUOTE_GROUNDING && r.content_hash == h)
        .unwrap();
    assert_eq!(live.metrics_json, rescored.metrics_json, "the key was overwritten");
    let r = &remediated(&env)[0];
    assert!(r.before.contains("0 of 1 quotes grounded"), "{}", r.before);
}

/// A claim made before the column existed says so, never reads the live row.
#[test]
fn a_claim_without_frozen_metrics_says_it_was_not_frozen() {
    let env = remediated_env(Some(Scores::AllPass));
    env.receipts
        .execute("UPDATE remediations SET trigger_metrics_json = NULL", [])
        .unwrap();
    let r = &remediated(&env)[0];
    assert!(r.before.contains("was not frozen"), "{}", r.before);
}

#[test]
fn outcome_still_failing_names_what_still_fails() {
    let env = remediated_env(Some(Scores::QuoteGroundingFails));
    let r = &remediated(&env)[0];
    assert_eq!(r.outcome, Outcome::StillFailing);
    assert!(
        r.after.contains("quote-grounding: 0 of 1 quotes grounded"),
        "{}",
        r.after
    );
}

#[test]
fn a_lost_post_response_is_pending_until_the_receipt_settles_then_follows_the_receipt() {
    // The claim never learned the trace (response lost); intake's row carries lineage.
    let env = Env::new();
    env.trace("orig", Scores::QuoteGroundingFails);
    env.claim("https://example.com/orig", "orig", None, Some("timed out"));
    env.remediation_receipt("rem-1", "orig", "https://example.com/orig");
    let r = &remediated(&env)[0];
    assert_eq!(r.remediation_trace.as_deref(), Some("rem-1"), "found through replay_of");
    assert_eq!(
        r.outcome,
        Outcome::Pending,
        "the POST error does not decide the outcome"
    );

    receipts::mark_failed(
        &env.receipts,
        "rem-1",
        vault::receipts::FailureStage::FetchFailed,
        "gone",
    )
    .unwrap();
    assert_eq!(remediated(&env)[0].outcome, Outcome::AttemptFailed);
}

#[test]
fn a_claim_with_no_trace_and_no_intake_row_is_pending_or_failed_by_its_post_error() {
    let env = Env::new();
    env.trace("orig", Scores::QuoteGroundingFails);
    env.claim("https://example.com/orig", "orig", None, None);
    assert_eq!(remediated(&env)[0].outcome, Outcome::Pending);

    let env = Env::new();
    env.trace("orig", Scores::QuoteGroundingFails);
    env.claim("https://example.com/orig", "orig", None, Some("connection reset"));
    assert_eq!(remediated(&env)[0].outcome, Outcome::AttemptFailed);
}

#[test]
fn the_remediated_section_renders_each_entry_with_before_and_after() {
    let env = remediated_env(Some(Scores::AllPass));
    let text = build(&env.inputs(&[]), &Canned::new("{\"categories\":[]}"), MODEL, date())
        .unwrap()
        .render();
    assert!(text.contains("- `orig` -> `rem-1`: **passed**"), "{text}");
    assert!(
        text.contains("  - before: quote-grounding: 0 of 1 quotes grounded"),
        "{text}"
    );
    assert!(text.contains("  - after: all 7 required checks pass"), "{text}");
}

// ---- candidates, writing, message, health ----

#[test]
fn candidates_missing_dir_is_none_and_an_unparseable_file_is_listed_loudly() {
    let env = Env::new();
    env.trace("tr-ok", Scores::AllPass);
    let g = gather(&env.inputs(&[])).unwrap();
    assert!(g.candidates.is_empty());

    let dir = env.reports.join("candidates");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("distill-video-2026-10-16.md"), "not a candidate").unwrap();
    std::fs::write(dir.join("notes.txt"), "ignored").unwrap();
    let g = gather(&env.inputs(&[])).unwrap();
    assert_eq!(g.candidates.len(), 1);
    assert!(matches!(&g.candidates[0], CandidateLine::Unreadable { error, .. } if error.contains("frontmatter")));
    let text = Report {
        date: date(),
        model: MODEL.to_string(),
        judge_models: vec![MODEL.to_string()],
        gathered: g,
        clustering: Clustering::default(),
        left_out: 0,
    }
    .render();
    assert!(text.contains("distill-video-2026-10-16.md` is unreadable"), "{text}");
}

#[test]
fn the_report_is_written_to_the_dated_file_and_replaces_the_days_earlier_one() {
    let env = Env::new();
    env.trace("tr-ok", Scores::AllPass);
    let report = build(&env.inputs(&[]), &Canned::new("unused"), MODEL, date()).unwrap();
    let path = report.write(&env.reports).unwrap();
    assert_eq!(path, env.reports.join("2026-10-17.md"));
    std::fs::write(&path, "stale").unwrap();
    report.write(&env.reports).unwrap();
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .starts_with("# Eval report 2026-10-17")
    );
}

#[test]
fn the_message_is_one_bounded_text_with_counts_top_categories_and_the_path() {
    let env = Env::new();
    for t in ["tr-a", "tr-b"] {
        env.trace(t, Scores::QuoteGroundingFails);
    }
    let clusterer = Canned::new(
        "{\"categories\":[{\"name\":\"Fabricated quotes\",\"description\":\"d\",\"traces\":[\"tr-a\",\"tr-b\"]}]}",
    );
    let report = build(&env.inputs(&[]), &clusterer, MODEL, date()).unwrap();
    let msg = report.message(Path::new("/data/eval-reports/2026-10-17.md"));
    assert!(
        msg.contains("2026-10-17: 2 failing of 2 scored (2 actionable)"),
        "{msg}"
    );
    assert!(msg.contains("Top: Fabricated quotes (2)"), "{msg}");
    assert!(msg.contains("Remediated: 0 (0 passed, 0 still failing)"), "{msg}");
    assert!(msg.contains("Untrusted judge checks: 2 of 2"), "{msg}");
    assert!(msg.ends_with("Report: /data/eval-reports/2026-10-17.md"), "{msg}");

    let mut huge = report;
    huge.clustering.clusters[0].name = "x".repeat(10_000);
    assert!(huge.message(Path::new("/p")).chars().count() <= 3500);
}

#[test]
fn health_counts_failures_trust_remediation_and_the_newest_report() {
    let env = Env::new();
    env.trace("orig", Scores::QuoteGroundingFails);
    env.claim("https://example.com/orig", "orig", Some("rem-1"), None);
    env.remediation_receipt("rem-1", "orig", "https://example.com/orig");
    stage(&env.root, "rem-1");
    receipts::mark_succeeded(&env.receipts, "rem-1", "inbox/y.md", false).unwrap();
    score(&env.store, &env.root, "rem-1", Scores::QuoteGroundingFails);
    std::fs::create_dir_all(&env.reports).unwrap();
    for name in ["2026-10-03.md", "2026-10-10.md", "notes.md", "2026-13-99.md"] {
        std::fs::write(env.reports.join(name), "x").unwrap();
    }

    let h = health(&env.inputs(&[])).unwrap();
    assert_eq!(h.scored, 2);
    assert_eq!(h.failing, 2);
    assert_eq!(h.actionable, 2);
    assert_eq!(h.judge_checks, 2);
    assert_eq!(h.untrusted_checks.len(), 2);
    assert_eq!(h.remediated, 1);
    assert_eq!(h.still_failing, 1);
    assert_eq!(h.last_report, Some(NaiveDate::from_ymd_opt(2026, 10, 10).unwrap()));
}

#[test]
fn latest_report_date_of_a_missing_dir_is_none() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(latest_report_date(&tmp.path().join("nope")).unwrap(), None);
}

#[test]
fn an_empty_judge_model_list_is_a_loud_error() {
    let mut env = Env::new();
    env.eval.judge_models.clear();
    let err = gather(&env.inputs(&[])).unwrap_err().to_string();
    assert!(err.contains("eval.judge-models is empty"), "{err}");
}

/// A remediated trace whose only non-failing evidence for judge-claims is a
/// not-judgeable row is never reported as passed.
#[test]
fn outcome_not_judgeable_when_judge_claims_could_not_read_the_source() {
    let env = remediated_env(Some(Scores::ClaimsNotJudgeable));
    let r = &remediated(&env)[0];
    assert_eq!(r.outcome, Outcome::NotJudgeable);
    assert_eq!(r.outcome.as_str(), "not-judgeable");
    assert!(
        r.after.contains(&format!(
            "judge-claims ({MODEL}) not judgeable: source 500000 chars > cap 400000"
        )),
        "{}",
        r.after
    );
    // Raise the cap past the source and the row stops covering: unscored.
    let required = required_checks(&env.eval.judge_models);
    let raised = remediated::collect(&env.receipts, &env.store, &env.root, &required, 600_000).unwrap();
    assert_eq!(raised[0].outcome, Outcome::Unscored);
}
