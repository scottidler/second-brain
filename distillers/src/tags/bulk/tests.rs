use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use super::*;
use crate::tags::spend::{FakeClock, SpendLedger, SpendRow};
use crate::tags::{CanonicalSet, TagMapping, TagsClassifierConfig, Transport};

/// A classifier.dev that charges a flat price per call, answers every label
/// as Jev, and can fail or refuse one call by number (1-based).
struct PricedService {
    usd_per_call: f64,
    /// `(call number, status)`: answer that call with this status instead.
    refuse: Option<(usize, u16)>,
    sent: Mutex<Vec<Vec<String>>>,
}

impl PricedService {
    fn charging(usd_per_call: f64) -> Arc<Self> {
        Arc::new(Self {
            usd_per_call,
            refuse: None,
            sent: Mutex::new(Vec::new()),
        })
    }

    fn refusing(usd_per_call: f64, call: usize, status: u16) -> Arc<Self> {
        Arc::new(Self {
            usd_per_call,
            refuse: Some((call, status)),
            sent: Mutex::new(Vec::new()),
        })
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.sent.lock().expect("lock").clone()
    }
}

impl Transport for PricedService {
    fn send(&self, body: &serde_json::Value, _token: Option<&str>) -> Result<(u16, String)> {
        let inputs: Vec<String> = body["inputs"]
            .as_array()
            .expect("inputs")
            .iter()
            .map(|v| v.as_str().expect("input").to_string())
            .collect();
        let labels: Vec<String> = body["labels"]
            .as_array()
            .expect("labels")
            .iter()
            .map(|v| v.as_str().expect("label").to_string())
            .collect();
        let call = {
            let mut sent = self.sent.lock().expect("lock");
            sent.push(inputs.clone());
            sent.len()
        };
        if let Some((n, status)) = self.refuse
            && n == call
        {
            let body = match status {
                429 => r#"{"error":{"code":"rate_limited"}}"#,
                402 => r#"{"error":{"code":"daily_spending_limit"}}"#,
                _ => r#"{"error":"upstream"}"#,
            };
            return Ok((status, body.to_string()));
        }
        let scores: serde_json::Map<String, serde_json::Value> =
            labels.iter().map(|l| (l.clone(), serde_json::json!(0.5))).collect();
        let results: Vec<serde_json::Value> = inputs
            .iter()
            .map(|_| serde_json::json!({"model": "jev-1.13.0", "scores": scores}))
            .collect();
        let reply = serde_json::json!({
            "results": results,
            "usage": {"input_tokens": 1_000 * inputs.len()},
            "pricing": {"estimated_usd": self.usd_per_call},
        });
        Ok((200, reply.to_string()))
    }
}

/// 100 long labels: one shard, and few enough notes per request that a test
/// sees several chunks without thousands of inputs.
fn one_shard_canon() -> CanonicalSet {
    CanonicalSet {
        all: (0..100).map(|i| format!("label-{i:03}-{}", "x".repeat(30))).collect(),
        no_segment: HashSet::new(),
        no_classifier: HashSet::new(),
        max_per_note: 8,
        descriptions: Default::default(),
    }
}

/// `repeats: 1`: these fixtures pin exact call counts and chunk sizes to
/// exercise `BulkRun`'s allowance and cursor mechanics (Phase 2), which
/// `repeats` (Phase 6) would multiply without changing what is under test.
fn dev_over(service: Arc<PricedService>) -> ClassifierDev {
    let cfg = TagsClassifierConfig {
        repeats: 1,
        ..TagsClassifierConfig::default()
    };
    ClassifierDev::with_transport(one_shard_canon(), TagMapping::new(), &cfg, service)
}

fn notes(n: usize) -> Vec<(String, String, String)> {
    (0..n)
        .map(|i| {
            (
                format!("notes/note-{i:04}.md"),
                format!("Note {i}"),
                format!("Summary of note {i}. {}", "words ".repeat(130)),
            )
        })
        .collect()
}

fn items(owned: &[(String, String, String)]) -> Vec<BulkItem<'_>> {
    owned
        .iter()
        .map(|(id, title, text)| BulkItem {
            id,
            input: TagInput {
                title,
                text,
                candidates: &[],
            },
        })
        .collect()
}

/// Notes per chunk for these fixtures, from the classifier's own planner.
fn chunk_size(dev: &ClassifierDev) -> usize {
    let owned = notes(400);
    let texts: Vec<String> = owned
        .iter()
        .map(|(_, title, text)| clip_classifier_text(title, text))
        .collect();
    let plan = dev.plan_run_chunks(&texts);
    assert!(plan.len() >= 4, "fixtures should span several chunks: {plan:?}");
    plan[0]
}

fn day_spend(dir: &Path, clock: Arc<FakeClock>, budget: f64, reserve: f64) -> DaySpend {
    let cfg = TagsClassifierConfig {
        daily_usd_budget: budget,
        ingest_reserve_usd: reserve,
        ..TagsClassifierConfig::default()
    };
    DaySpend::new(SpendLedger::new(dir.join("classifier-spend.jsonl")), clock, &cfg)
}

fn ledger_rows(dir: &Path) -> Vec<SpendRow> {
    match std::fs::read_to_string(dir.join("classifier-spend.jsonl")) {
        Ok(text) => text.lines().map(|l| serde_json::from_str(l).expect("row")).collect(),
        Err(_) => Vec::new(),
    }
}

fn run_once(
    dev: &ClassifierDev,
    spend: &DaySpend,
    owned: &[(String, String, String)],
    cursor_path: &Path,
    max_usd: Option<f64>,
) -> BulkOutcome {
    let run = BulkRun {
        classifier: dev,
        spend,
        max_usd,
        path: "tag-apply",
        tag: Some("ci-cd"),
        keep: None,
    };
    let mut cursor = ScoreCursor::load(cursor_path).expect("load cursor");
    run.run(&items(owned), &mut cursor, &mut |c| c.save(cursor_path))
        .expect("a stopped run is still Ok")
}

/// Success criterion 1: $0.05 of allowance against a $0.02-per-chunk service
/// scores two chunks, persists them, and returns Ok naming the remainder.
#[test]
fn a_run_spends_the_allowance_then_stops_and_reports_the_remainder() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cursor_path = tmp.path().join("cursor/ci-cd.json");
    let service = PricedService::charging(0.02);
    let dev = dev_over(service.clone());
    let size = chunk_size(&dev);
    let owned = notes(4 * size);
    let spend = day_spend(tmp.path(), FakeClock::at("2026-09-26"), 0.15, 0.10);

    let outcome = run_once(&dev, &spend, &owned, &cursor_path, None);

    assert_eq!(service.calls().len(), 2, "two chunks sent");
    assert_eq!(outcome.scored, 2 * size);
    assert_eq!(outcome.remaining, 2 * size);
    assert!(
        matches!(outcome.stop, Some(BulkStop::DayAllowance { .. })),
        "{outcome:?}"
    );
    assert!((outcome.spent_usd - 0.04).abs() < 1e-12);
    let report = outcome.to_string();
    assert!(
        report.contains(&format!("{} remaining, resumes after 00:00 UTC", 2 * size)),
        "{report}"
    );

    let persisted = ScoreCursor::load(&cursor_path).expect("reload");
    assert_eq!(persisted.scores.len(), 2 * size, "both chunks persisted");
    assert_eq!(persisted.scoring_config_hash, dev.scoring_config_hash());
    let rows = ledger_rows(tmp.path());
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .all(|r| r.path == "tag-apply" && r.tag.as_deref() == Some("ci-cd"))
    );
    assert!(rows.iter().all(|r| r.day.to_string() == "2026-09-26" && r.usd == 0.02));
}

/// Success criterion 2: at the budget the same day, a run makes zero calls;
/// the next UTC day it sends only the notes the cursor lacks.
#[test]
fn a_spent_day_makes_no_calls_and_the_next_day_scores_only_what_is_missing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cursor_path = tmp.path().join("ci-cd.json");
    let clock = FakeClock::at("2026-09-26");
    let spend = day_spend(tmp.path(), clock.clone(), 0.15, 0.10);
    let first = PricedService::charging(0.02);
    let dev = dev_over(first.clone());
    let size = chunk_size(&dev);
    let owned = notes(4 * size);
    run_once(&dev, &spend, &owned, &cursor_path, None);
    let after_first = ScoreCursor::load(&cursor_path).expect("reload");

    // Top the day up to the budget: $0.04 by the run, $0.11 from elsewhere.
    spend
        .ledger()
        .append(&[SpendRow {
            day: "2026-09-26".parse().expect("date"),
            usd: 0.11,
            tokens: 0,
            path: "retag".to_string(),
            tag: None,
        }])
        .expect("append");
    let same_day = PricedService::charging(0.02);
    let outcome = run_once(&dev_over(same_day.clone()), &spend, &owned, &cursor_path, None);
    assert!(same_day.calls().is_empty(), "no HTTP call on a spent day");
    assert_eq!(outcome.scored, 0);
    assert_eq!(outcome.remaining, 2 * size);
    assert_eq!(outcome.stop, Some(BulkStop::DayAllowance { available_usd: 0.0 }));
    assert_eq!(ScoreCursor::load(&cursor_path).expect("reload"), after_first);

    clock.advance_one_day();
    let next_day = PricedService::charging(0.02);
    let outcome = run_once(&dev_over(next_day.clone()), &spend, &owned, &cursor_path, None);
    let sent: HashSet<String> = next_day.calls().into_iter().flatten().collect();
    let lacked: HashSet<String> = owned
        .iter()
        .filter(|(id, _, _)| !after_first.scores.contains_key(id))
        .map(|(_, title, text)| clip_classifier_text(title, text))
        .collect();
    assert_eq!(sent, lacked, "only the notes the cursor lacked were sent");
    assert_eq!(next_day.calls().iter().map(Vec::len).sum::<usize>(), 2 * size);
    assert_eq!(outcome.scored, 2 * size);
    assert_eq!(outcome.remaining, 0);
    assert_eq!(outcome.stop, None);
    assert_eq!(ScoreCursor::load(&cursor_path).expect("reload").scores.len(), 4 * size);
}

/// Success criterion 3: the third chunk fails; the ledger still holds the
/// first two, and the failed chunk is recorded as failed, not scored.
#[test]
fn a_failed_third_chunk_leaves_the_ledger_rows_of_the_first_two() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cursor_path = tmp.path().join("ci-cd.json");
    let service = PricedService::refusing(0.02, 3, 500);
    let dev = dev_over(service.clone());
    let size = chunk_size(&dev);
    let owned = notes(3 * size);
    let spend = day_spend(tmp.path(), FakeClock::at("2026-09-26"), 0.50, 0.10);

    let outcome = run_once(&dev, &spend, &owned, &cursor_path, None);

    assert_eq!(service.calls().len(), 3);
    assert_eq!(ledger_rows(tmp.path()).len(), 2, "one row per successful call");
    assert_eq!(outcome.scored, 2 * size);
    assert_eq!(outcome.failed, size);
    let persisted = ScoreCursor::load(&cursor_path).expect("reload");
    assert_eq!(persisted.scores.len(), 2 * size);
    assert_eq!(persisted.failed.len(), size);
    assert!(
        persisted.failed.values().all(|e| e.contains("500")),
        "{:?}",
        persisted.failed
    );
}

/// classifier.dev's own limit is the backstop: the run stops at it, keeps
/// what it paid for, and sends nothing after.
#[test]
fn a_service_limit_stops_the_run_without_retrying_into_it() {
    for status in [429, 402] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cursor_path = tmp.path().join("ci-cd.json");
        let service = PricedService::refusing(0.02, 2, status);
        let dev = dev_over(service.clone());
        let size = chunk_size(&dev);
        let owned = notes(4 * size);
        let spend = day_spend(tmp.path(), FakeClock::at("2026-09-26"), 0.50, 0.10);

        let outcome = run_once(&dev, &spend, &owned, &cursor_path, None);

        assert_eq!(service.calls().len(), 2, "{status}: nothing sent after the refusal");
        assert!(matches!(outcome.stop, Some(BulkStop::ServiceLimit(_))), "{outcome:?}");
        assert_eq!(outcome.scored, size);
        assert_eq!(outcome.failed, 0, "refused notes stay pending, not failed");
        assert_eq!(outcome.remaining, 3 * size);
        assert!(outcome.to_string().contains("resumes after 00:00 UTC"));
        let persisted = ScoreCursor::load(&cursor_path).expect("reload");
        assert_eq!(persisted.scores.len(), size);
        assert!(persisted.failed.is_empty());
    }
}

#[test]
fn max_usd_caps_a_run_below_the_allowance() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cursor_path = tmp.path().join("ci-cd.json");
    let service = PricedService::charging(0.02);
    let dev = dev_over(service.clone());
    let size = chunk_size(&dev);
    let owned = notes(4 * size);
    let spend = day_spend(tmp.path(), FakeClock::at("2026-09-26"), 0.50, 0.10);

    let outcome = run_once(&dev, &spend, &owned, &cursor_path, Some(0.03));

    assert_eq!(service.calls().len(), 1);
    assert_eq!(outcome.stop, Some(BulkStop::MaxUsd { max_usd: 0.03 }));
    assert!(outcome.to_string().contains("rerun to resume"), "{outcome}");
}

/// A stored score is reused only for the same wire text under the same
/// scoring config.
#[test]
fn an_edited_note_is_rescored_and_a_new_config_discards_every_score() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cursor_path = tmp.path().join("ci-cd.json");
    let spend = day_spend(tmp.path(), FakeClock::at("2026-09-26"), 5.0, 0.10);
    let mut owned = notes(5);
    run_once(
        &dev_over(PricedService::charging(0.001)),
        &spend,
        &owned,
        &cursor_path,
        None,
    );

    owned[3].2.push_str(" An edit.");
    let service = PricedService::charging(0.001);
    run_once(&dev_over(service.clone()), &spend, &owned, &cursor_path, None);
    assert_eq!(
        service.calls(),
        vec![vec![clip_classifier_text(&owned[3].1, &owned[3].2)]]
    );

    let mut cursor = ScoreCursor::load(&cursor_path).expect("reload");
    assert_eq!(cursor.scores.len(), 5);
    assert!(cursor.reconcile("a different scoring config"));
    assert!(cursor.scores.is_empty());
    assert!(
        !cursor.reconcile("a different scoring config"),
        "same config keeps scores"
    );
}

#[test]
fn keep_limits_the_slugs_stored_per_note() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spend = day_spend(tmp.path(), FakeClock::at("2026-09-26"), 5.0, 0.10);
    let dev = dev_over(PricedService::charging(0.001));
    let owned = notes(2);
    let keep = vec!["label-007-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".to_string()];
    let run = BulkRun {
        classifier: &dev,
        spend: &spend,
        max_usd: None,
        path: "tag-apply",
        tag: None,
        keep: Some(&keep),
    };
    let mut cursor = ScoreCursor::default();
    run.run(&items(&owned), &mut cursor, &mut |_| Ok(())).expect("run");
    for score in cursor.scores.values() {
        assert_eq!(score.scores.keys().cloned().collect::<Vec<_>>(), keep);
    }
}

#[test]
fn a_missing_cursor_loads_empty_and_a_corrupt_one_fails() {
    let tmp = tempfile::tempdir().expect("tempdir");
    assert_eq!(
        ScoreCursor::load(&tmp.path().join("none.json")).expect("load"),
        ScoreCursor::default()
    );
    let bad = tmp.path().join("bad.json");
    std::fs::write(&bad, "{not json").expect("write");
    assert!(ScoreCursor::load(&bad).is_err());
}
