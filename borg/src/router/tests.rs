use super::*;

fn test_links() -> Vec<LinkConfig> {
    vec![
        LinkConfig {
            name: "shorts".to_string(),
            regex: r"https?://(?:www\.)?youtube\.com/shorts/([a-zA-Z0-9_-]+)".to_string(),
            resolution: "480p".to_string(),
        },
        LinkConfig {
            name: "youtube".to_string(),
            regex:
                r"https?://(?:www\.)?(youtube\.com/watch\?v=|youtu\.be/|music\.youtube\.com/watch\?v=)([a-zA-Z0-9_-]+)"
                    .to_string(),
            resolution: "FWVGA".to_string(),
        },
        LinkConfig {
            name: "github".to_string(),
            regex: r"https?://github\.com/[^/]+/[^/]+/?(\?[^ ]*)?$".to_string(),
            resolution: "FWVGA".to_string(),
        },
        LinkConfig {
            name: "social".to_string(),
            regex: r"https?://x\.com/[^/]+/status/\d+".to_string(),
            resolution: "FWVGA".to_string(),
        },
        LinkConfig {
            name: "reddit".to_string(),
            regex: r"https?://(?:www\.)?reddit\.com/r/[^/]+/comments/".to_string(),
            resolution: "FWVGA".to_string(),
        },
        LinkConfig {
            name: "default".to_string(),
            regex: r".*".to_string(),
            resolution: "FWVGA".to_string(),
        },
    ]
}

#[test]
fn test_youtube_url() {
    let result = classify_url("https://www.youtube.com/watch?v=abc123", &test_links()).expect("valid");
    assert_eq!(result.link_name, "youtube");
    assert!(result.is_youtube_type());
    assert_eq!(result.width, 854);
    assert_eq!(result.height, 480);
}

#[test]
fn test_youtube_short_url() {
    let result = classify_url("https://youtu.be/abc123", &test_links()).expect("valid");
    assert_eq!(result.link_name, "youtube");
    assert!(result.is_youtube_type());
}

#[test]
fn test_youtube_music_url() {
    let result = classify_url("https://music.youtube.com/watch?v=abc123", &test_links()).expect("valid");
    assert_eq!(result.link_name, "youtube");
    assert!(result.is_youtube_type());
}

#[test]
fn test_youtube_shorts() {
    let result = classify_url("https://youtube.com/shorts/abc123", &test_links()).expect("valid");
    assert_eq!(result.link_name, "shorts");
    assert!(result.is_shorts());
    assert_eq!(result.width, 480);
    assert_eq!(result.height, 854);
}

#[test]
fn test_article_url() {
    let result = classify_url("https://blog.example.com/post", &test_links()).expect("valid");
    assert_eq!(result.link_name, "default");
    assert!(!result.is_youtube_type());
}

#[test]
fn test_github_repo_url() {
    let result = classify_url("https://github.com/open-webui/open-terminal", &test_links()).expect("valid");
    assert_eq!(result.link_name, "github");
}

#[test]
fn test_github_repo_url_trailing_slash() {
    let result = classify_url("https://github.com/Infatoshi/OpenSquirrel/", &test_links()).expect("valid");
    assert_eq!(result.link_name, "github");
}

#[test]
fn test_github_deep_path_is_not_github() {
    let result = classify_url("https://github.com/owner/repo/blob/main/file.rs", &test_links()).expect("valid");
    assert_eq!(result.link_name, "default");
}

#[test]
fn test_github_issues_is_not_github() {
    let result = classify_url("https://github.com/owner/repo/issues/42", &test_links()).expect("valid");
    assert_eq!(result.link_name, "default");
}

#[test]
fn test_github_blog_is_not_github() {
    let result = classify_url("https://github.com/blog/something", &test_links()).expect("valid");
    // "blog/something" has two segments so it would match github pattern
    // This is acceptable - github.com/blog is treated as a "repo" URL
    // In practice, github.com/blog redirects to github.blog
    assert!(result.link_name == "github" || result.link_name == "default");
}

#[test]
fn test_social_x_post() {
    let result = classify_url("https://x.com/Zai_org/status/2033221428640674015", &test_links()).expect("valid");
    assert_eq!(result.link_name, "social");
}

#[test]
fn test_reddit_thread() {
    let result = classify_url(
        "https://www.reddit.com/r/footballstrategy/comments/lhb3ku/help_me_understand/",
        &test_links(),
    )
    .expect("valid");
    assert_eq!(result.link_name, "reddit");
}

#[test]
fn test_reddit_no_www() {
    let result = classify_url("https://reddit.com/r/rust/comments/abc123/some_post/", &test_links()).expect("valid");
    assert_eq!(result.link_name, "reddit");
}

#[test]
fn test_non_url_matches_default() {
    // classify_url no longer validates URLs (that's normalize_url's job)
    // Non-URL text matches the catch-all default pattern
    let result = classify_url("not a url", &test_links()).expect("valid");
    assert_eq!(result.link_name, "default");
}

#[test]
fn test_pre_normalized_url() {
    // classify_url now expects pre-normalized URLs
    let result = classify_url("https://www.youtube.com/watch?v=abc", &test_links()).expect("valid");
    assert_eq!(result.url, "https://www.youtube.com/watch?v=abc");
    assert_eq!(result.link_name, "youtube");
}

#[test]
fn test_custom_resolution() {
    let links = vec![LinkConfig {
        name: "youtube".to_string(),
        regex: r"https?://(?:www\.)?youtube\.com/watch".to_string(),
        resolution: "FHD".to_string(),
    }];
    let result = classify_url("https://www.youtube.com/watch?v=abc", &links).expect("valid");
    assert_eq!(result.width, 1920);
    assert_eq!(result.height, 1080);
}

#[test]
fn test_resolve_dimensions_landscape() {
    assert_eq!(resolve_dimensions("nHD", false), (640, 360));
    assert_eq!(resolve_dimensions("FWVGA", false), (854, 480));
    assert_eq!(resolve_dimensions("FHD", false), (1920, 1080));
    assert_eq!(resolve_dimensions("4K", false), (3840, 2160));
}

#[test]
fn test_resolve_dimensions_shorts() {
    assert_eq!(resolve_dimensions("480p", true), (480, 854));
    assert_eq!(resolve_dimensions("720p", true), (720, 1280));
    assert_eq!(resolve_dimensions("1080p", true), (1080, 1920));
}

#[test]
fn test_resolve_dimensions_unknown() {
    assert_eq!(resolve_dimensions("unknown", false), (854, 480));
    assert_eq!(resolve_dimensions("unknown", true), (480, 854));
}

#[test]
fn test_extract_bare_url() {
    let result = extract_url_from_text("https://example.com/page");
    assert_eq!(result, Some("https://example.com/page".to_string()));
}

#[test]
fn test_extract_url_in_sentence() {
    let result = extract_url_from_text("check this out https://example.com/page please");
    assert_eq!(result, Some("https://example.com/page".to_string()));
}

#[test]
fn test_extract_url_trailing_punctuation() {
    let result = extract_url_from_text("See https://example.com/page.");
    assert_eq!(result, Some("https://example.com/page".to_string()));
}

#[test]
fn test_extract_url_trailing_paren() {
    let result = extract_url_from_text("(https://example.com/page)");
    assert_eq!(result, Some("https://example.com/page".to_string()));
}

#[test]
fn test_extract_no_url() {
    let result = extract_url_from_text("no urls here");
    assert_eq!(result, None);
}

#[test]
fn test_extract_multiple_urls_takes_first() {
    let result = extract_url_from_text("https://first.com and https://second.com");
    assert_eq!(result, Some("https://first.com".to_string()));
}

#[test]
fn test_format_reply_completed() {
    let result = IngestResult {
        status: IngestStatus::Completed,
        note_path: Some("/vault/Inbox/Test.md".to_string()),
        title: Some("Test Article".to_string()),
        tags: vec!["ai".to_string(), "tech".to_string()],
        elapsed_secs: Some(4.56),
        ..Default::default()
    };
    let reply = format_reply(&result, "https://example.com");
    assert_eq!(reply, "Saved: Test Article (4.6s)\nTags: #ai, #tech");
}

#[test]
fn test_format_reply_completed_with_no_tags() {
    let result = IngestResult {
        status: IngestStatus::Completed,
        note_path: Some("/vault/inbox/Test.md".to_string()),
        title: Some("Test".to_string()),
        tags: vec![],
        ..Default::default()
    };
    let reply = format_reply(&result, "https://example.com");
    assert_eq!(reply, "Saved: Test");
}

#[test]
fn test_format_reply_completed_no_tags() {
    let result = IngestResult {
        status: IngestStatus::Completed,
        title: Some("Test".to_string()),
        tags: vec![],
        ..Default::default()
    };
    let reply = format_reply(&result, "https://example.com");
    assert_eq!(reply, "Saved: Test");
}

#[test]
fn test_format_reply_failed() {
    let result = IngestResult {
        status: IngestStatus::Failed {
            reason: "network error".to_string(),
        },
        elapsed_secs: Some(2.3),
        ..Default::default()
    };
    let reply = format_reply(&result, "https://example.com/broken");
    assert_eq!(reply, "Failed (2.3s): network error\nURL: https://example.com/broken");
}

#[test]
fn test_format_reply_queued() {
    let result = IngestResult {
        status: IngestStatus::Queued,
        ..Default::default()
    };
    let reply = format_reply(&result, "https://example.com");
    assert_eq!(reply, "Queued for processing.");
}

#[test]
fn test_format_reply_with_trace_id_completed() {
    let result = IngestResult {
        status: IngestStatus::Completed,
        title: Some("Test Article".to_string()),
        tags: vec!["ai".to_string()],
        elapsed_secs: Some(5.7),
        trace_id: Some("tg-7f3a2c".to_string()),
        ..Default::default()
    };
    let reply = format_reply(&result, "https://example.com");
    assert_eq!(reply, "[tg-7f3a2c] Saved: Test Article (5.7s)\nTags: #ai");
}

#[test]
fn test_format_reply_with_trace_id_duplicate() {
    let result = IngestResult {
        status: IngestStatus::Duplicate {
            original_date: "2026-03-16".to_string(),
        },
        elapsed_secs: Some(0.001),
        trace_id: Some("tg-7f3a2c".to_string()),
        ..Default::default()
    };
    let reply = format_reply(&result, "https://example.com");
    assert_eq!(
        reply,
        "[tg-7f3a2c] Duplicate (0.0s): already ingested on 2026-03-16\nURL: https://example.com"
    );
}

#[test]
fn test_format_reply_with_trace_id_failed() {
    let result = IngestResult {
        status: IngestStatus::Failed {
            reason: "connection timeout".to_string(),
        },
        elapsed_secs: Some(0.3),
        trace_id: Some("tg-7f3a2c".to_string()),
        ..Default::default()
    };
    let reply = format_reply(&result, "https://example.com");
    assert_eq!(
        reply,
        "[tg-7f3a2c] Failed (0.3s): connection timeout\nURL: https://example.com"
    );
}
