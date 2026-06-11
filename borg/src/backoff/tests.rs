use super::*;

#[test]
fn test_cap() {
    let backoff = ExponentialBackoff::new();
    assert_eq!(backoff.cap, Duration::from_secs(30));
    assert_eq!(backoff.base, Duration::from_secs(1));
}

#[test]
fn test_reset() {
    let mut backoff = ExponentialBackoff::new();
    backoff.attempt = 5;
    backoff.reset();
    assert_eq!(backoff.attempt, 0);
}

#[test]
fn reset_if_healthy_resets_only_after_threshold() {
    let mut backoff = ExponentialBackoff::new();

    // A connection that just started is NOT healthy yet - backoff grows.
    backoff.attempt = 5;
    backoff.reset_if_healthy(Instant::now());
    assert_eq!(backoff.attempt, 5, "fast drop must not reset the backoff");

    // A connection that has been up past the threshold resets.
    backoff.attempt = 5;
    let long_ago = Instant::now()
        .checked_sub(Duration::from_secs(HEALTHY_RUN_SECS + 1))
        .expect("instant in range");
    backoff.reset_if_healthy(long_ago);
    assert_eq!(backoff.attempt, 0, "sustained-healthy run must reset the backoff");
}
