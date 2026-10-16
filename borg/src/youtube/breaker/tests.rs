use super::*;

#[test]
fn a_streak_pauses_and_a_success_resets_it() {
    let now = Instant::now();
    let mut b = Breaker::new(3, Duration::from_secs(60));
    assert!(!b.record_failure(now));
    assert!(!b.record_failure(now));
    b.record_success();
    assert!(!b.record_failure(now), "the success reset the streak");
    assert!(!b.record_failure(now));
    assert!(b.record_failure(now), "third in a row opens the pause");
    assert_eq!(b.paused_for(now), Some(Duration::from_secs(60)));
}

#[test]
fn the_pause_expires() {
    let now = Instant::now();
    let mut b = Breaker::new(1, Duration::from_secs(60));
    assert!(b.record_failure(now));
    assert!(b.paused_for(now + Duration::from_secs(59)).is_some());
    assert!(b.paused_for(now + Duration::from_secs(60)).is_none());
}

#[test]
fn failures_while_paused_do_not_extend_it() {
    let now = Instant::now();
    let mut b = Breaker::new(1, Duration::from_secs(60));
    assert!(b.record_failure(now));
    assert!(!b.record_failure(now + Duration::from_secs(30)));
    assert!(b.paused_for(now + Duration::from_secs(60)).is_none());
}
