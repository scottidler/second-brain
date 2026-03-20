// Content quality gate - detects blocked/garbage content before note creation.

/// Known block page title patterns (high confidence - these are almost never real titles)
const BLOCKED_TITLE_INDICATORS: &[&str] = &[
    "just a moment",
    "attention required",
    "access denied",
    "one more step",
    "please verify you are a human",
];

/// Known block page content patterns (require short content to trigger)
const BLOCKED_CONTENT_INDICATORS: &[&str] = &[
    "checking your browser",
    "enable javascript and cookies",
    "ray id:",
    "cf-browser-verification",
    "please turn javascript on",
    "captcha",
    "sucuri website firewall",
    "ddos protection by",
];

/// Check fetched content for signs of blocked/garbage responses.
/// Returns Some(reason) if the content appears to be blocked, None if it looks legitimate.
pub fn detect_blocked_content(content: &str, title: &str) -> Option<String> {
    let lower_title = title.to_lowercase();

    // Check title for known block page titles (high confidence)
    for indicator in BLOCKED_TITLE_INDICATORS {
        if lower_title.contains(indicator) {
            return Some(format!("Blocked content detected in title: '{title}'"));
        }
    }

    // Check if content is suspiciously short combined with block indicators in the body
    let trimmed = content.trim();
    if trimmed.len() < 500 {
        let lower_content = trimmed.to_lowercase();
        for indicator in BLOCKED_CONTENT_INDICATORS {
            if lower_content.contains(indicator) {
                return Some(format!(
                    "Blocked content detected: short content ({} chars) with '{indicator}'",
                    trimmed.len()
                ));
            }
        }
    }

    // Check if title is a raw URL (fetch failed to extract a real title)
    if lower_title.starts_with("http://") || lower_title.starts_with("https://") {
        return Some(format!("Title is a raw URL, content fetch likely failed: '{title}'"));
    }

    None
}

#[cfg(test)]
mod tests {
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
    fn test_case_insensitive_content() {
        let result = detect_blocked_content("CHECKING YOUR BROWSER before accessing", "Title");
        assert!(result.is_some());
    }
}
