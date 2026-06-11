use super::*;

#[test]
fn test_detect_cloudflare_title() {
    let result = detect_blocked_content("some content", "Just a moment...");
    assert!(result.is_some());
    assert!(result.as_ref().is_some_and(|r| r.contains("Blocked content")));
}

#[test]
fn test_detect_attention_required() {
    let result = detect_blocked_content("short", "Attention Required! | Cloudflare");
    assert!(result.is_some());
}

#[test]
fn test_detect_access_denied() {
    let result = detect_blocked_content("", "Access Denied");
    assert!(result.is_some());
}

#[test]
fn test_detect_short_content_with_indicator() {
    let content = "Please enable JavaScript and cookies to continue. Ray ID: abc123";
    let result = detect_blocked_content(content, "Some Title");
    assert!(result.is_some());
    assert!(result.as_ref().is_some_and(|r| r.contains("short content")));
}

#[test]
fn test_long_content_with_indicator_is_ok() {
    // Legitimate article about Cloudflare that mentions "ray id:" but is long enough
    let content = "x".repeat(600) + " ray id: abc123";
    let result = detect_blocked_content(&content, "How Cloudflare Works");
    assert!(result.is_none());
}

#[test]
fn test_raw_url_title() {
    let result = detect_blocked_content(
        "some actual content here that is long enough",
        "https://github.com/NousResearch/hermes-agent",
    );
    assert!(result.is_some());
    assert!(result.as_ref().is_some_and(|r| r.contains("raw URL")));
}

#[test]
fn test_legitimate_content_passes() {
    let content = "This is a real article about technology. ".repeat(20);
    let result = detect_blocked_content(&content, "A Real Article Title");
    assert!(result.is_none());
}

#[test]
fn test_empty_content_without_indicators_passes() {
    // Short content but no block indicators - could be a legitimate short page
    let result = detect_blocked_content("Short but real.", "A Title");
    assert!(result.is_none());
}

#[test]
fn test_captcha_in_short_content() {
    let result = detect_blocked_content("Please complete the captcha below", "Verify");
    assert!(result.is_some());
}

#[test]
fn test_case_insensitive_title() {
    let result = detect_blocked_content("content", "JUST A MOMENT...");
    assert!(result.is_some());
}

#[test]
fn test_detect_github_auth_wall_title() {
    // GitHub's universal login redirect serves this title for any
    // unauthenticated scrape that hits an auth wall. Without this
    // indicator the gate would pass auth-wall bodies through.
    let body = "x".repeat(600); // long enough to bypass the short-content path
    let result = detect_blocked_content(
        &body,
        "Search code, repositories, users, issues, pull requests · GitHub",
    );
    assert!(result.is_some());
    assert!(result.as_ref().is_some_and(|r| r.contains("Blocked content")));
}

#[test]
fn test_case_insensitive_content() {
    let result = detect_blocked_content("CHECKING YOUR BROWSER before accessing", "Title");
    assert!(result.is_some());
}

#[test]
fn test_detect_truncation_not_detailed() {
    let summary = "1. **Basic Prompting** - Just you and a prompt.\n\
                        6. [Not detailed in provided content] - Advanced level.";
    let result = detect_truncation_artifacts(summary);
    assert!(result.is_some());
    assert!(result.as_ref().is_some_and(|r| r.contains("Truncation artifact")));
}

#[test]
fn test_detect_truncation_case_insensitive() {
    let summary = "Some text [NOT DETAILED IN PROVIDED CONTENT] more text";
    let result = detect_truncation_artifacts(summary);
    assert!(result.is_some());
}

#[test]
fn test_clean_summary_passes_truncation_check() {
    let summary = "## Key Ideas\n\n- **Great idea** - This is well explained.\n\
                        - **Another idea** - Also well covered.";
    let result = detect_truncation_artifacts(summary);
    assert!(result.is_none());
}
