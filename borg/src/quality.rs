// Content quality gate - detects blocked/garbage content before note creation.

/// Patterns that indicate the LLM couldn't see the full content (truncation artifacts).
const TRUNCATION_INDICATORS: &[&str] = &[
    "[not detailed in provided content]",
    "[not available in provided content]",
    "[not available in transcript]",
    "[content not provided]",
    "[not mentioned in provided content]",
    "[not included in provided content]",
    "[information not available]",
    "[details not provided]",
    "[not covered in provided content]",
    "[not in provided content]",
    "[not specified in provided content]",
];

/// Check summarized output for signs of truncation artifacts.
/// Returns Some(reason) if the output appears to contain placeholder content
/// from an LLM that didn't see the full input.
pub fn detect_truncation_artifacts(summary: &str) -> Option<String> {
    let lower = summary.to_lowercase();
    for indicator in TRUNCATION_INDICATORS {
        if lower.contains(indicator) {
            return Some(format!("Truncation artifact detected: summary contains '{indicator}'"));
        }
    }
    None
}

/// Known block page title patterns (high confidence - these are almost never real titles)
const BLOCKED_TITLE_INDICATORS: &[&str] = &[
    "just a moment",
    "attention required",
    "access denied",
    "one more step",
    "please verify you are a human",
    "search code, repositories, users, issues, pull requests",
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
mod tests;
