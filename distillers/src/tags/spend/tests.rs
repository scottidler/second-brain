use super::*;

fn usage(usd: f64, ok: bool) -> CallUsage {
    CallUsage {
        input_tokens: if ok { 100_418 } else { 0 },
        estimated_usd: usd,
        ok,
    }
}

#[test]
fn the_row_matches_the_design_docs_line() {
    let row = SpendRow {
        day: "2026-09-26".parse().expect("date"),
        usd: 0.0042,
        tokens: 100_418,
        path: "tag-apply".to_string(),
        tag: Some("ci-cd".to_string()),
    };
    assert_eq!(
        serde_json::to_string(&row).expect("json"),
        r#"{"day":"2026-09-26","usd":0.0042,"tokens":100418,"path":"tag-apply","tag":"ci-cd"}"#
    );
    let untagged = SpendRow {
        tag: None,
        path: "retag".to_string(),
        ..row
    };
    assert!(!serde_json::to_string(&untagged).expect("json").contains("\"tag\""));
}

#[test]
fn a_missing_ledger_is_a_day_with_no_spend() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ledger = SpendLedger::new(tmp.path().join("nested/classifier-spend.jsonl"));
    assert_eq!(
        ledger.day_total("2026-09-26".parse().expect("date")).expect("total"),
        0.0
    );
}

#[test]
fn day_total_sums_only_that_utc_day() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ledger = SpendLedger::new(tmp.path().join("sb/classifier-spend.jsonl"));
    let clock = FakeClock::at("2026-09-26");
    let spend = DaySpend::new(ledger.clone(), clock.clone(), &TagsClassifierConfig::default());

    spend
        .record(
            &[usage(0.02, true), usage(0.0, false), usage(0.01, true)],
            "retag",
            None,
        )
        .expect("record");
    clock.advance_one_day();
    spend
        .record(&[usage(0.04, true)], "tag-apply", Some("ci-cd"))
        .expect("record");

    let lines = std::fs::read_to_string(ledger.path()).expect("read");
    assert_eq!(lines.lines().count(), 3, "the failed call writes no row:\n{lines}");
    assert!((ledger.day_total("2026-09-26".parse().expect("date")).expect("total") - 0.03).abs() < 1e-12);
    assert!((ledger.day_total("2026-09-27".parse().expect("date")).expect("total") - 0.04).abs() < 1e-12);
}

#[test]
fn a_malformed_line_fails_loudly() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("classifier-spend.jsonl");
    std::fs::write(
        &path,
        "{\"day\":\"2026-09-26\",\"usd\":0.01,\"tokens\":1,\"path\":\"retag\"}\nnot json\n",
    )
    .expect("write");
    let err = SpendLedger::new(&path)
        .day_total("2026-09-26".parse().expect("date"))
        .expect_err("a corrupt ledger must not read as low spend");
    assert!(err.to_string().contains(":2:"), "{err}");
}

#[test]
fn the_allowance_is_budget_less_spent_less_reserve() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let ledger = SpendLedger::new(tmp.path().join("classifier-spend.jsonl"));
    let spend = DaySpend::new(ledger, FakeClock::at("2026-09-26"), &TagsClassifierConfig::default());

    let fresh = spend.allowance().expect("allowance");
    assert!((fresh.available_usd() - 0.40).abs() < 1e-12, "{fresh}");

    spend.record(&[usage(0.35, true)], "retag", None).expect("record");
    let spent = spend.allowance().expect("allowance");
    assert!((spent.available_usd() - 0.05).abs() < 1e-12, "{spent}");

    spend.record(&[usage(0.20, true)], "retag", None).expect("record");
    let over = spend.allowance().expect("allowance");
    assert_eq!(over.available_usd(), 0.0, "never negative: {over}");
    assert!(over.to_string().contains("2026-09-26 UTC"), "{over}");
}
