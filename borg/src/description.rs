use std::sync::LazyLock;

use regex::Regex;

static HASHTAG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"#([\w][\w-]*)").expect("valid hashtag regex"));

static SECTION_KILLERS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?i)follow me on",
        r"(?i)let's connect",
        r"(?i)connect with me",
        r"(?i)social media links",
        r"(?i)for business inquiries",
        r"(?i)my main.*channel",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("valid section killer regex"))
    .collect()
});

static LINE_KILLERS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"sub_confirmation",
        r"(?i)subscribe for more",
        r"(?i)watch my most recent upload",
        r"(?i)consider becoming a patron",
        r"patreon\.com",
        r"(?i)sponsored by",
        r"(?i)affiliate",
        r"promo=",
        r"(?i)teespring\.com",
        r"(?i)\bmerch\b",
        r"(?i)if you find my content helpful",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("valid line killer regex"))
    .collect()
});

/// Regex to detect decorative separator lines (only emoji, whitespace, dashes, no alphanumeric).
static DECORATOR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[^\w]*$").expect("valid decorator regex"));

/// Extract hashtags from description text.
///
/// Returns lowercase, deduplicated tags with `#` stripped.
pub fn extract_hashtags(description: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut tags = Vec::new();
    for cap in HASHTAG_RE.captures_iter(description) {
        let tag = cap[1].to_lowercase();
        if seen.insert(tag.clone()) {
            tags.push(tag);
        }
    }
    tags
}

/// Filter a YouTube description to remove boilerplate noise.
///
/// Uses a state machine with section killers (kill-to-end) and line killers
/// (drop individual lines). Returns `None` if the result is empty.
pub fn filter_description(description: &str) -> Option<String> {
    let mut result_lines: Vec<String> = Vec::new();
    let mut in_killed_section = false;

    for line in description.lines() {
        // Section killers: once triggered, all remaining lines are dropped
        if !in_killed_section && SECTION_KILLERS.iter().any(|re| re.is_match(line)) {
            in_killed_section = true;
            continue;
        }

        if in_killed_section {
            continue;
        }

        // Line killers: drop individual lines
        if LINE_KILLERS.iter().any(|re| re.is_match(line)) {
            continue;
        }

        // Decorative separators: only emoji + whitespace + dashes, no alphanumeric
        let trimmed = line.trim();
        if !trimmed.is_empty() && DECORATOR_RE.is_match(trimmed) {
            continue;
        }

        // Strip hashtags from the text (they've been extracted into tags)
        let cleaned = HASHTAG_RE.replace_all(line, "").to_string();
        result_lines.push(cleaned);
    }

    // Post-processing: collapse runs of 3+ blank lines to 2
    let mut final_lines: Vec<String> = Vec::new();
    let mut consecutive_blanks = 0;
    for line in &result_lines {
        if line.trim().is_empty() {
            consecutive_blanks += 1;
            if consecutive_blanks <= 2 {
                final_lines.push(line.clone());
            }
        } else {
            consecutive_blanks = 0;
            final_lines.push(line.clone());
        }
    }

    // Trim leading/trailing whitespace
    let text = final_lines.join("\n").trim().to_string();

    if text.is_empty() { None } else { Some(text) }
}

#[cfg(test)]
mod tests;
