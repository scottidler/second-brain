use super::*;

const LIMITED: &str = "ERROR: [youtube] abc: Video unavailable";
/// yt-dlp joins YouTube's reason and subreason, so a dead video can carry the
/// same "Video unavailable" the rate limit does; the subreason tells them apart.
const DEAD: &str = "ERROR: [youtube] abc: Video unavailable. This video has been removed by the uploader";

#[test]
fn only_ambiguous_or_rate_limit_errors_count() {
    assert!(counts_toward_pause(LIMITED));
    assert!(counts_toward_pause(
        "Video unavailable. This content isn't available, try again later. Your account has been rate-limited"
    ));
    assert!(counts_toward_pause("Sign in to confirm you're not a bot"));
    assert!(!counts_toward_pause(DEAD));
    assert!(!counts_toward_pause("ERROR: [youtube] abc: This video is unavailable"));
    assert!(!counts_toward_pause(
        "ERROR: [youtube] abc: Requested format is not available. Use --list-formats"
    ));
    assert!(!counts_toward_pause("ERROR: [youtube] abc: Private video"));
    assert!(!counts_toward_pause(
        "ERROR: [youtube] abc: Video unavailable. This video is no longer available because the YouTube account associated with this video has been terminated."
    ));
}

/// 2026-10-16: five known-dead videos at the head of a re-run paused all
/// YouTube fetches for 30 minutes. Permanent errors never count.
#[test]
fn permanent_errors_never_pause() {
    let now = Instant::now();
    let mut b = Breaker::new(3, Duration::from_secs(60));
    for _ in 0..10 {
        assert_eq!(b.record_failure(DEAD, now), Verdict::Continue);
    }
    assert!(b.paused_for(now).is_none());
}

#[test]
fn a_streak_with_a_good_canary_resets() {
    let now = Instant::now();
    let mut b = Breaker::new(2, Duration::from_secs(60));
    b.record_success("https://youtu.be/good");
    assert_eq!(b.record_failure(LIMITED, now), Verdict::Continue);
    assert_eq!(
        b.record_failure(LIMITED, now),
        Verdict::Canary("https://youtu.be/good".to_string())
    );
    assert_eq!(
        b.record_failure(LIMITED, now),
        Verdict::Continue,
        "one canary at a time"
    );
    b.canary_result(true, now);
    assert!(b.paused_for(now).is_none(), "dead videos, not a limit");
    assert_eq!(
        b.record_failure(LIMITED, now),
        Verdict::Continue,
        "the streak restarted"
    );
}

#[test]
fn a_streak_with_a_failing_canary_pauses_until_it_expires() {
    let now = Instant::now();
    let mut b = Breaker::new(1, Duration::from_secs(60));
    b.record_success("https://youtu.be/good");
    assert!(matches!(b.record_failure(LIMITED, now), Verdict::Canary(_)));
    b.canary_result(false, now);
    assert_eq!(b.paused_for(now), Some(Duration::from_secs(60)));
    assert_eq!(
        b.record_failure(LIMITED, now),
        Verdict::Continue,
        "no extension while paused"
    );
    assert!(b.paused_for(now + Duration::from_secs(60)).is_none());
}

#[test]
fn a_streak_with_no_known_good_video_pauses() {
    let now = Instant::now();
    let mut b = Breaker::new(2, Duration::from_secs(60));
    assert_eq!(b.record_failure(LIMITED, now), Verdict::Continue);
    assert_eq!(b.record_failure(LIMITED, now), Verdict::Paused);
    assert!(b.paused_for(now).is_some());
}

#[test]
fn a_success_resets_the_streak() {
    let now = Instant::now();
    let mut b = Breaker::new(2, Duration::from_secs(60));
    assert_eq!(b.record_failure(LIMITED, now), Verdict::Continue);
    b.record_success("https://youtu.be/good");
    assert_eq!(b.record_failure(LIMITED, now), Verdict::Continue);
}
