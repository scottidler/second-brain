use super::*;

#[test]
fn hit_serializes_kebab_case_and_skips_absent_counts() {
    let hit = ChannelHit {
        id: "FUg1y-yv6cs".into(),
        title: "How Rust engineered the perfect async runtime".into(),
        url: "https://www.youtube.com/watch?v=FUg1y-yv6cs".into(),
        duration_secs: Some(488),
        view_count: None,
    };
    let json = serde_json::to_value(&hit).expect("json");
    assert_eq!(json["duration-secs"], 488);
    assert!(json.get("view-count").is_none(), "absent count is skipped: {json}");
    let back: ChannelHit = serde_json::from_value(json).expect("round trip");
    assert_eq!(back, hit);
}

#[test]
fn failure_carries_null_stderr_tail_when_none_was_captured() {
    let failure = ChannelSearchFailure {
        error: "yt-dlp timed out after 8s".into(),
        stderr_tail: None,
    };
    let json = serde_json::to_value(&failure).expect("json");
    assert_eq!(
        json,
        serde_json::json!({"error": "yt-dlp timed out after 8s", "stderr-tail": null})
    );
}
