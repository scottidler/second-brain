#![allow(clippy::unwrap_used)]

use super::*;

#[test]
fn evaluate_empty_returns_ok() {
    assert!(evaluate_findings(std::iter::empty()).is_ok());
}

#[test]
fn evaluate_only_ok_returns_ok() {
    let findings = [Finding::ok("everything is fine")];
    assert!(evaluate_findings(findings.iter()).is_ok());
}

#[test]
fn evaluate_only_info_returns_ok() {
    let findings = [Finding::info("did you know")];
    assert!(evaluate_findings(findings.iter()).is_ok());
}

#[test]
fn evaluate_only_warn_returns_ok() {
    let findings = [
        Finding::warn("something is off", "run sb foo to fix"),
        Finding::warn("another thing", "check the docs"),
    ];
    assert!(
        evaluate_findings(findings.iter()).is_ok(),
        "warnings must not cause a non-zero exit",
    );
}

#[test]
fn evaluate_warn_plus_error_returns_err() {
    let findings = [
        Finding::warn("ignorable", "see notes"),
        Finding::error("show stopper", "run sb fix-everything"),
    ];
    let err = evaluate_findings(findings.iter()).expect_err("must err on error severity");
    assert!(err.to_string().contains("1 error-severity"), "{err}");
}

#[test]
fn evaluate_multiple_errors_counts_them() {
    let findings = [
        Finding::error("a", "fix a"),
        Finding::error("b", "fix b"),
        Finding::error("c", "fix c"),
    ];
    let err = evaluate_findings(findings.iter()).expect_err("must err on error severity");
    assert!(err.to_string().contains("3 error-severity"), "{err}");
}
