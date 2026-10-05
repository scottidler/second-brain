use super::*;

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

#[test]
fn reconnect_schedule_is_one_second_base_thirty_second_cap() {
    let backoff = ExponentialBackoff::reconnect();
    assert_eq!(backoff.cap, secs(30));
    assert_eq!(backoff.base, secs(1));
}

#[test]
fn test_reset() {
    let mut backoff = ExponentialBackoff::reconnect();
    backoff.attempt = 5;
    backoff.reset();
    assert_eq!(backoff.attempt, 0);
}

#[test]
fn reset_if_healthy_resets_only_after_threshold() {
    let mut backoff = ExponentialBackoff::reconnect();

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

#[test]
fn unhinted_schedule_is_1_2_4_then_capped_at_20() {
    let mut backoff = ExponentialBackoff::new(secs(1), secs(20));
    let delays: Vec<Duration> = (0..7).map(|_| backoff.next_delay(None)).collect();
    assert_eq!(delays, [1, 2, 4, 8, 16, 20, 20].map(secs));
}

#[test]
fn a_hint_is_used_and_capped_and_the_attempt_advances() {
    let mut backoff = ExponentialBackoff::new(secs(1), secs(20));
    assert_eq!(backoff.next_delay(Some(secs(3))), secs(3));
    assert_eq!(backoff.attempts(), 1, "a hinted delay still counts as an attempt");
    assert_eq!(
        backoff.next_delay(Some(secs(3600))),
        secs(20),
        "a 3600 s hint is capped"
    );
    assert_eq!(backoff.attempts(), 2);
    assert_eq!(
        backoff.next_delay(None),
        secs(4),
        "the unhinted schedule continues from attempt 2"
    );
}

fn at(date: &str) -> SystemTime {
    httpdate::parse_http_date(date).expect("fixture date")
}

#[test]
fn retry_after_header_parses_delta_seconds() {
    assert_eq!(retry_after_header("120", SystemTime::UNIX_EPOCH), Some(secs(120)));
    assert_eq!(retry_after_header(" 7 ", SystemTime::UNIX_EPOCH), Some(secs(7)));
}

#[test]
fn retry_after_header_parses_an_http_date_relative_to_now() {
    let now = at("Wed, 21 Oct 2026 07:26:00 GMT");
    assert_eq!(
        retry_after_header("Wed, 21 Oct 2026 07:28:00 GMT", now),
        Some(secs(120))
    );
}

#[test]
fn retry_after_header_date_in_the_past_is_a_zero_wait() {
    let now = at("Wed, 21 Oct 2026 07:30:00 GMT");
    assert_eq!(
        retry_after_header("Wed, 21 Oct 2026 07:28:00 GMT", now),
        Some(Duration::ZERO)
    );
}

#[test]
fn retry_after_header_garbage_is_none() {
    for bad in ["", "soon", "-5", "1.5", "Wed, 99 Oct 2026 07:28:00 GMT"] {
        assert_eq!(retry_after_header(bad, SystemTime::UNIX_EPOCH), None, "{bad:?}");
    }
}
