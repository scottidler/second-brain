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
mod tests {
    use super::*;

    // ── extract_hashtags tests ──

    #[test]
    fn test_extract_hashtags_empty() {
        assert!(extract_hashtags("").is_empty());
    }

    #[test]
    fn test_extract_hashtags_no_hashtags() {
        assert!(extract_hashtags("Just a normal description with no tags").is_empty());
    }

    #[test]
    fn test_extract_hashtags_basic() {
        let tags = extract_hashtags("Check out #Rust and #programming tips");
        assert_eq!(tags, vec!["rust", "programming"]);
    }

    #[test]
    fn test_extract_hashtags_with_hyphens() {
        let tags = extract_hashtags("#claude-code #home-lab #k8s");
        assert_eq!(tags, vec!["claude-code", "home-lab", "k8s"]);
    }

    #[test]
    fn test_extract_hashtags_dedup() {
        let tags = extract_hashtags("#Rust #rust #RUST");
        assert_eq!(tags, vec!["rust"]);
    }

    #[test]
    fn test_extract_hashtags_inline() {
        let tags = extract_hashtags("My #HomeLab tour after 3 years #Kubernetes #homelab");
        assert_eq!(tags, vec!["homelab", "kubernetes"]);
    }

    // ── filter_description tests ──

    #[test]
    fn test_filter_empty() {
        assert_eq!(filter_description(""), None);
    }

    #[test]
    fn test_filter_whitespace_only() {
        assert_eq!(filter_description("   \n\n   "), None);
    }

    #[test]
    fn test_filter_keeps_opening_paragraph() {
        let desc = "Kubernetes homelab tour after 3 years";
        assert_eq!(filter_description(desc), Some(desc.to_string()));
    }

    #[test]
    fn test_filter_section_killer_kills_to_end() {
        let desc = "Recorded live on twitch, GET IN\n\
                     \n\
                     https://twitch.tv/ThePrimeagen\n\
                     \n\
                     MY MAIN YT CHANNEL: Has well edited engineering videos\n\
                     https://youtube.com/ThePrimeagen\n\
                     \n\
                     Discord\n\
                     https://discord.gg/ThePrimeagen";
        let filtered = filter_description(desc).expect("should produce filtered output");
        assert!(filtered.contains("Recorded live on twitch"));
        assert!(filtered.contains("twitch.tv/ThePrimeagen"));
        assert!(!filtered.contains("MY MAIN YT CHANNEL"));
        assert!(!filtered.contains("Discord"));
    }

    #[test]
    fn test_filter_line_killers() {
        let desc = "Great video about Rust\n\
                     \n\
                     https://boot.dev/?promo=PRIMEYT\n\
                     \n\
                     Subscribe for more content!\n\
                     \n\
                     Reviewed video: https://youtube.com/watch?v=abc";
        let filtered = filter_description(desc).expect("should produce filtered output");
        assert!(filtered.contains("Great video about Rust"));
        assert!(!filtered.contains("promo="));
        assert!(!filtered.contains("Subscribe for more"));
        assert!(filtered.contains("Reviewed video"));
    }

    #[test]
    fn test_filter_primeagen_example() {
        let desc = "Recorded live on twitch, GET IN\n\
                     \n\
                     https://twitch.tv/ThePrimeagen\n\
                     \n\
                     Become a backend engineer.  Its my favorite site\n\
                     https://boot.dev/?promo=PRIMEYT\n\
                     \n\
                     This is also the best way to support me is to support yourself becoming a better backend engineer.\n\
                     \n\
                     Reviewed video: https://www.youtube.com/watch?v=1UEMXDSh8Og\n\
                     By: https://www.youtube.com/@awesome-coding\n\
                     \n\
                     MY MAIN YT CHANNEL: Has well edited engineering videos\n\
                     https://youtube.com/ThePrimeagen\n\
                     \n\
                     Discord\n\
                     https://discord.gg/ThePrimeagen\n\
                     \n\
                     Have something for me to read or react to?: https://www.reddit.com/r/ThePrimeagenReact/\n\
                     \n\
                     Kinesis Advantage 360: https://bit.ly/Prime-Kinesis\n\
                     \n\
                     Hey I am sponsored by Turso, an edge database.\n\
                     https://turso.tech/deeznuts";
        let filtered = filter_description(desc).expect("should produce filtered output");
        assert!(filtered.contains("Recorded live on twitch, GET IN"));
        assert!(filtered.contains("twitch.tv/ThePrimeagen"));
        assert!(!filtered.contains("promo="));
        assert!(filtered.contains("Reviewed video"));
        assert!(filtered.contains("@awesome-coding"));
        assert!(!filtered.contains("MY MAIN YT CHANNEL"));
        assert!(!filtered.contains("Discord"));
        assert!(!filtered.contains("Kinesis"));
        assert!(!filtered.contains("Turso"));
    }

    #[test]
    fn test_filter_strips_hashtags() {
        let desc = "My homelab tour #HomeLab #Kubernetes #selfhosted\n\nResources below";
        let filtered = filter_description(desc).expect("should produce filtered output");
        assert!(!filtered.contains('#'));
        assert!(filtered.contains("My homelab tour"));
        assert!(filtered.contains("Resources below"));
    }

    #[test]
    fn test_filter_collapses_blank_lines() {
        let desc = "Line one\n\n\n\n\nLine two";
        let filtered = filter_description(desc).expect("should produce filtered output");
        // 2 blank lines = 3 newlines max between content lines; 4+ newlines means 3+ blanks
        assert!(
            !filtered.contains("\n\n\n\n"),
            "should have at most 2 consecutive blank lines"
        );
        assert!(filtered.contains("Line one"));
        assert!(filtered.contains("Line two"));
    }

    #[test]
    fn test_filter_affiliate_line() {
        let desc = "Great tool for coding\n\
                     Use my affiliate link: https://example.com\n\
                     More content here";
        let filtered = filter_description(desc).expect("should produce filtered output");
        assert!(!filtered.contains("affiliate"));
        assert!(filtered.contains("Great tool"));
        assert!(filtered.contains("More content here"));
    }

    #[test]
    fn test_filter_sponsored_line() {
        let desc = "Today's video\n\
                     Sponsored by NordVPN\n\
                     Back to the content";
        let filtered = filter_description(desc).expect("should produce filtered output");
        assert!(!filtered.contains("Sponsored by"));
        assert!(filtered.contains("Today's video"));
        assert!(filtered.contains("Back to the content"));
    }

    #[test]
    fn test_filter_patreon_line() {
        let desc = "Good stuff\n\
                     Consider becoming a patron: https://patreon.com/creator\n\
                     More good stuff";
        let filtered = filter_description(desc).expect("should produce filtered output");
        assert!(!filtered.contains("patreon"));
        assert!(!filtered.contains("patron"));
    }

    #[test]
    fn test_filter_lets_connect() {
        let desc = "Main content here\n\
                     \n\
                     Let's connect!\n\
                     Twitter: @handle\n\
                     Instagram: @handle";
        let filtered = filter_description(desc).expect("should produce filtered output");
        assert!(filtered.contains("Main content here"));
        assert!(!filtered.contains("connect"));
        assert!(!filtered.contains("Twitter"));
    }
}
