use super::*;
use crate::cli::checks::Severity;

fn day(d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, d).unwrap()
}

fn health() -> Health {
    Health {
        scored: 33,
        failing: 12,
        actionable: 3,
        judge_checks: 4,
        untrusted_checks: Vec::new(),
        remediated: 2,
        still_failing: 0,
        last_report: Some(day(14)),
    }
}

fn warns(findings: &[Finding]) -> Vec<&Finding> {
    findings.iter().filter(|f| f.severity == Severity::Warn).collect()
}

#[test]
fn the_eval_line_states_counts_trust_remediation_and_the_last_report() {
    let findings = eval_findings(&health(), 8, day(17));
    assert_eq!(findings.len(), 1, "a healthy loop is one line: {findings:?}");
    assert_eq!(findings[0].severity, Severity::Info);
    assert_eq!(
        findings[0].message,
        "eval: 33 traces scored, 12 failing (3 actionable), 4 of 4 judge checks trusted, 2 remediated (0 still failing), last weekly report 2026-10-14"
    );
}

#[test]
fn an_untrusted_judge_check_is_a_warn_naming_the_checks() {
    let mut h = health();
    h.untrusted_checks = vec![
        "judge-claims (gemini-2.5-pro)".to_string(),
        "judge-distillation (gemini-2.5-pro)".to_string(),
    ];
    let findings = eval_findings(&h, 8, day(17));
    assert!(
        findings[0].message.contains("2 of 4 judge checks trusted"),
        "{}",
        findings[0].message
    );
    let warns = warns(&findings);
    assert_eq!(warns.len(), 1);
    assert!(
        warns[0].message.contains("2 of 4 judge checks are untrusted"),
        "{}",
        warns[0].message
    );
    assert!(
        warns[0].message.contains("judge-claims (gemini-2.5-pro)"),
        "{}",
        warns[0].message
    );
}

#[test]
fn a_remediated_note_that_still_fails_is_a_warn() {
    let mut h = health();
    h.still_failing = 1;
    let findings = eval_findings(&h, 8, day(17));
    let warns = warns(&findings);
    assert_eq!(warns.len(), 1);
    assert!(
        warns[0].message.contains("1 remediated note(s) still fail"),
        "{}",
        warns[0].message
    );
}

#[test]
fn failing_traces_alone_are_not_a_warn() {
    let mut h = health();
    h.failing = 40;
    h.actionable = 40;
    assert!(warns(&eval_findings(&h, 8, day(17))).is_empty());
}

#[test]
fn a_missing_report_is_a_warn_and_a_stale_one_is_a_warn_at_the_boundary_only() {
    let mut h = health();
    h.last_report = None;
    let findings = eval_findings(&h, 8, day(17));
    assert!(
        findings[0].message.ends_with("last weekly report never"),
        "{}",
        findings[0].message
    );
    assert!(
        warns(&findings)[0]
            .message
            .contains("no weekly report has been written")
    );

    let mut h = health();
    h.last_report = Some(day(9));
    assert!(
        warns(&eval_findings(&h, 8, day(17))).is_empty(),
        "8 days old is within 8"
    );
    let stale = eval_findings(&h, 8, day(18));
    assert!(warns(&stale)[0].message.contains("9 days old"), "{stale:?}");
}

#[test]
fn nothing_scored_is_one_info_line_not_a_warn() {
    let mut h = health();
    h.scored = 0;
    h.last_report = None;
    let findings = eval_findings(&h, 8, day(17));
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].severity, Severity::Info);
    assert!(findings[0].message.contains("no traces scored yet"));
}
