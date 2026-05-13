use super::*;

#[test]
fn validate_cap_accepts_min() {
    assert!(validate_cap("k", MIN_CAP).is_ok());
}

#[test]
fn validate_cap_accepts_max() {
    assert!(validate_cap("k", MAX_CAP).is_ok());
}

#[test]
fn validate_cap_rejects_zero() {
    let err = validate_cap("max-concurrent-traces", 0).expect_err("zero must fail");
    let msg = format!("{err}");
    assert!(msg.contains("max-concurrent-traces"));
    assert!(msg.contains("0"));
}

#[test]
fn validate_cap_rejects_above_max() {
    let err = validate_cap("max-concurrent-heavy-traces", MAX_CAP + 1).expect_err("oversize must fail");
    let msg = format!("{err}");
    assert!(msg.contains("max-concurrent-heavy-traces"));
}
