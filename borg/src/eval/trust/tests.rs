use super::*;
use crate::eval::cache::{CLAIMS_RUBRIC_VERSION, RUBRIC_VERSION};
use crate::eval::inject::ClassTally;
use crate::eval::{CHECK_JUDGE_CLAIMS, CHECK_JUDGE_DISTILLATION};

const MODEL: &str = "gemini-2.5-pro";

fn policy() -> TrustConfig {
    TrustConfig::default()
}

fn row(check: &str, class: PlantedClass, paired: f64, n: usize) -> TrustRow {
    TrustRow {
        judge_model: MODEL.to_string(),
        check_name: check.to_string(),
        planted_class: class.as_str().to_string(),
        rubric_version: rubric_version_of(check).unwrap().to_string(),
        paired_catch_rate: paired,
        raw_catch_rate: paired,
        flag_rate: 0.0,
        wilson_lower: 0.0,
        sample_size: n,
        measured_at: "2026-10-17T00:00:00Z".to_string(),
    }
}

/// A row for every class `check` can see, each at `paired` over `n`.
fn every_class(check: &str, paired: f64, n: usize) -> Vec<TrustRow> {
    applicable_classes(check)
        .into_iter()
        .map(|c| row(check, c, paired, n))
        .collect()
}

/// A row built from a tally, the way `run_inject` writes it.
fn row_from(check: &str, class: PlantedClass, t: ClassTally) -> TrustRow {
    TrustRow {
        paired_catch_rate: t.paired_catch_rate(),
        raw_catch_rate: t.raw_catch_rate(),
        flag_rate: t.flag_rate(),
        wilson_lower: t.wilson_lower(),
        ..row(check, class, 0.0, t.n)
    }
}

fn failing_classes(state: &TrustState) -> Vec<PlantedClass> {
    match state {
        TrustState::Untrusted(failures) => failures.iter().map(|f| f.class).collect(),
        other => panic!("expected untrusted, got {other:?}"),
    }
}

#[test]
fn a_0_79_paired_catch_rate_on_one_applicable_class_is_untrusted() {
    let mut rows = every_class(CHECK_JUDGE_DISTILLATION, 0.90, 30);
    rows[2].paired_catch_rate = 0.79;
    let state = judge_check_trust(MODEL, CHECK_JUDGE_DISTILLATION, &rows, &policy());
    assert_eq!(failing_classes(&state), vec![PlantedClass::DroppedTopClaim]);
    assert!(!state.may_act());
}

#[test]
fn a_0_80_paired_catch_rate_on_every_applicable_class_with_30_samples_is_trusted() {
    let rows = every_class(CHECK_JUDGE_DISTILLATION, 0.80, 30);
    assert_eq!(
        judge_check_trust(MODEL, CHECK_JUDGE_DISTILLATION, &rows, &policy()),
        TrustState::Trusted
    );
    // 24 of 30 computed through the tally lands exactly on the floor.
    let mut t = ClassTally::default();
    for i in 0..30 {
        t.add(Some(false), Some(i < 24));
    }
    assert_eq!(t.paired_catch_rate(), 0.80);
    let rows: Vec<TrustRow> = applicable_classes(CHECK_JUDGE_DISTILLATION)
        .into_iter()
        .map(|c| row_from(CHECK_JUDGE_DISTILLATION, c, t))
        .collect();
    assert_eq!(
        judge_check_trust(MODEL, CHECK_JUDGE_DISTILLATION, &rows, &policy()),
        TrustState::Trusted
    );
}

#[test]
fn one_of_one_caught_is_untrusted_below_min_samples() {
    let mut t = ClassTally::default();
    t.add(Some(false), Some(true));
    assert_eq!(t.paired_catch_rate(), 1.0);
    let rows = vec![row_from(CHECK_JUDGE_CLAIMS, PlantedClass::NumberSwap, t)];
    let state = judge_check_trust(MODEL, CHECK_JUDGE_CLAIMS, &rows, &policy());
    match &state {
        TrustState::Untrusted(failures) => {
            assert_eq!(failures.len(), 1);
            assert!(failures[0].reason.contains("min-samples"), "{failures:?}");
        }
        other => panic!("expected untrusted, got {other:?}"),
    }
}

#[test]
fn a_check_that_flags_every_note_scores_paired_zero_and_is_untrusted() {
    let mut t = ClassTally::default();
    for _ in 0..40 {
        t.add(Some(true), Some(true));
    }
    assert_eq!(t.raw_catch_rate(), 1.0);
    assert_eq!(t.flag_rate(), 1.0);
    assert_eq!(t.paired_catch_rate(), 0.0);
    let rows = vec![row_from(CHECK_JUDGE_CLAIMS, PlantedClass::NumberSwap, t)];
    let state = judge_check_trust(MODEL, CHECK_JUDGE_CLAIMS, &rows, &policy());
    assert_eq!(failing_classes(&state), vec![PlantedClass::NumberSwap]);
}

#[test]
fn a_row_for_an_older_rubric_version_is_untrusted() {
    let mut rows = every_class(CHECK_JUDGE_DISTILLATION, 0.95, 40);
    for r in &mut rows {
        r.rubric_version = "v1".to_string();
    }
    assert_ne!(RUBRIC_VERSION, "v1");
    let state = judge_check_trust(MODEL, CHECK_JUDGE_DISTILLATION, &rows, &policy());
    assert_eq!(failing_classes(&state), PlantedClass::ALL.to_vec());
    let TrustState::Untrusted(failures) = state else {
        unreachable!()
    };
    assert!(failures[0].reason.contains("rubric v1"), "{failures:?}");
}

#[test]
fn judge_claims_is_never_untrusted_for_missing_a_who_change_or_fabricated_quote() {
    // It misses every who-change and fabricated quote, and was never measured
    // on a dropped claim: none of those cells is one it can see.
    let mut rows = vec![row(CHECK_JUDGE_CLAIMS, PlantedClass::NumberSwap, 0.85, 30)];
    rows.push(row(CHECK_JUDGE_CLAIMS, PlantedClass::WhoChange, 0.0, 30));
    rows.push(row(CHECK_JUDGE_CLAIMS, PlantedClass::FabricatedQuote, 0.0, 30));
    assert_eq!(CLAIMS_RUBRIC_VERSION, rows[0].rubric_version);
    assert_eq!(
        judge_check_trust(MODEL, CHECK_JUDGE_CLAIMS, &rows, &policy()),
        TrustState::Trusted
    );
}

#[test]
fn a_class_never_measured_is_untrusted_and_another_judges_rows_do_not_count() {
    let mut rows = every_class(CHECK_JUDGE_DISTILLATION, 0.95, 40);
    rows.retain(|r| r.planted_class != PlantedClass::WhoChange.as_str());
    let mut other = row(CHECK_JUDGE_DISTILLATION, PlantedClass::WhoChange, 0.95, 40);
    other.judge_model = "claude-sonnet-4-5".to_string();
    rows.push(other);
    let state = judge_check_trust(MODEL, CHECK_JUDGE_DISTILLATION, &rows, &policy());
    let TrustState::Untrusted(failures) = &state else {
        panic!("expected untrusted, got {state:?}")
    };
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].class, PlantedClass::WhoChange);
    assert_eq!(failures[0].reason, "never measured");
}

#[test]
fn a_deterministic_check_is_ungated() {
    let state = judge_check_trust("", crate::eval::scores::CHECK_QUOTE_GROUNDING, &[], &policy());
    assert_eq!(state, TrustState::Ungated);
    assert!(state.may_act());
}

#[test]
fn the_store_keeps_one_row_per_key_and_rubric_versions_side_by_side() {
    let store = TrustStore::open_memory().unwrap();
    let mut first = row(CHECK_JUDGE_CLAIMS, PlantedClass::NumberSwap, 0.5, 30);
    store.put(&first).unwrap();
    first.paired_catch_rate = 0.9;
    store.put(&first).unwrap();
    let mut old = first.clone();
    old.rubric_version = "v1".to_string();
    store.put(&old).unwrap();
    let rows = store.all().unwrap();
    assert_eq!(rows.len(), 2);
    let current = rows.iter().find(|r| r.rubric_version == CLAIMS_RUBRIC_VERSION).unwrap();
    assert_eq!(current.paired_catch_rate, 0.9);
}

#[test]
fn the_trust_table_shares_a_file_with_the_score_table_and_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("eval-cache.db");
    let _cache = crate::eval::cache::JudgmentCache::open(&path).unwrap();
    let _scores = crate::eval::scores::ScoreStore::open(&path).unwrap();
    let trust = TrustStore::open(&path).unwrap();
    trust
        .put(&row(CHECK_JUDGE_CLAIMS, PlantedClass::NumberSwap, 0.9, 30))
        .unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<std::result::Result<_, _>>()
        .unwrap();
    assert_eq!(
        tables,
        ["claim_judgments", "distill_judgments", "eval_scores", "judge_trust"]
    );
}
