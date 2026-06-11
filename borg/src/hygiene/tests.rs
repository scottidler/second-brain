use super::*;
use crate::config::default_canonicalization_rules;

#[test]
fn test_clean_url_strips_youtube_ephemeral() {
    let url = "https://www.youtube.com/watch?v=abc&t=13s&list=PLxyz&index=3";
    let cleaned = clean_url(url).expect("valid url");
    assert_eq!(cleaned, "https://www.youtube.com/watch?v=abc");
}

#[test]
fn test_clean_url_strips_start_radio_flow_app() {
    let url = "https://www.youtube.com/watch?v=abc&start_radio=1&flow=1&app=desktop";
    let cleaned = clean_url(url).expect("valid url");
    assert_eq!(cleaned, "https://www.youtube.com/watch?v=abc");
}

#[test]
fn test_canonicalize_youtu_be() {
    let rules = default_canonicalization_rules();
    let result = canonicalize_url("https://youtu.be/abc123", &rules);
    assert_eq!(result, "https://www.youtube.com/watch?v=abc123");
}

#[test]
fn test_canonicalize_mobile_youtube() {
    let rules = default_canonicalization_rules();
    let result = canonicalize_url("https://m.youtube.com/watch?v=abc123", &rules);
    assert_eq!(result, "https://www.youtube.com/watch?v=abc123");
}

#[test]
fn test_canonicalize_music_youtube() {
    let rules = default_canonicalization_rules();
    let result = canonicalize_url("https://music.youtube.com/watch?v=abc123", &rules);
    assert_eq!(result, "https://www.youtube.com/watch?v=abc123");
}

#[test]
fn test_canonicalize_youtube_nocookie() {
    let rules = default_canonicalization_rules();
    let result = canonicalize_url("https://www.youtube-nocookie.com/embed/abc123", &rules);
    assert_eq!(result, "https://www.youtube.com/watch?v=abc123");
}

#[test]
fn test_canonicalize_mobile_shorts() {
    let rules = default_canonicalization_rules();
    let result = canonicalize_url("https://m.youtube.com/shorts/abc123", &rules);
    assert_eq!(result, "https://www.youtube.com/shorts/abc123");
}

#[test]
fn test_canonicalize_twitter_to_x() {
    let rules = default_canonicalization_rules();
    let result = canonicalize_url("https://twitter.com/user/status/123", &rules);
    assert_eq!(result, "https://x.com/user/status/123");
}

#[test]
fn test_canonicalize_mobile_twitter_to_x() {
    let rules = default_canonicalization_rules();
    let result = canonicalize_url("https://mobile.twitter.com/user/status/123", &rules);
    assert_eq!(result, "https://x.com/user/status/123");
}

#[test]
fn test_canonicalize_no_match_passthrough() {
    let rules = default_canonicalization_rules();
    let result = canonicalize_url("https://example.com/page", &rules);
    assert_eq!(result, "https://example.com/page");
}

#[test]
fn test_canonicalize_www_youtube_unchanged() {
    let rules = default_canonicalization_rules();
    let result = canonicalize_url("https://www.youtube.com/watch?v=abc123", &rules);
    // www.youtube.com doesn't match any canonicalization rule - passthrough
    assert_eq!(result, "https://www.youtube.com/watch?v=abc123");
}

#[test]
fn test_normalize_url_full_pipeline() {
    let rules = default_canonicalization_rules();
    let result = normalize_url("https://youtu.be/abc123?si=tracking&t=45s", &rules).expect("valid");
    assert_eq!(result, "https://www.youtube.com/watch?v=abc123");
}

#[test]
fn test_canonicalize_custom_rule() {
    let rules = vec![CanonicalRule {
        name: "old-reddit".to_string(),
        match_regex: r"https?://old\.reddit\.com/(?P<path>.*)".to_string(),
        canonical: "https://www.reddit.com/{path}".to_string(),
    }];
    let result = canonicalize_url("https://old.reddit.com/r/rust/top", &rules);
    assert_eq!(result, "https://www.reddit.com/r/rust/top");
}

#[test]
fn test_clean_url_strips_utm() {
    let url = "https://example.com/page?utm_source=twitter&utm_medium=social&id=42";
    let cleaned = clean_url(url).expect("valid url");
    assert_eq!(cleaned, "https://example.com/page?id=42");
}

#[test]
fn test_clean_url_strips_all_tracking() {
    let url = "https://example.com/page?utm_source=x&fbclid=abc&gclid=def";
    let cleaned = clean_url(url).expect("valid url");
    assert_eq!(cleaned, "https://example.com/page");
}

#[test]
fn test_clean_url_preserves_non_tracking() {
    let url = "https://youtube.com/watch?v=abc123";
    let cleaned = clean_url(url).expect("valid url");
    assert_eq!(cleaned, "https://youtube.com/watch?v=abc123");
}

#[test]
fn test_clean_url_strips_youtube_si() {
    let url = "https://www.youtube.com/watch?v=abc&si=tracking123&pp=stuff";
    let cleaned = clean_url(url).expect("valid url");
    assert_eq!(cleaned, "https://www.youtube.com/watch?v=abc");
}

#[test]
fn test_clean_url_no_query() {
    let url = "https://example.com/page";
    let cleaned = clean_url(url).expect("valid url");
    assert_eq!(cleaned, "https://example.com/page");
}

#[test]
fn test_clean_url_invalid() {
    let result = clean_url("not a url");
    assert!(result.is_err());
}
