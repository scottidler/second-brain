use crate::schema::Domain;

/// Legacy folder-to-domain mapping for backward compat with old Fabric patterns
/// and any other code that might emit the old emoji folder paths.
const DOMAIN_ALIASES: &[(&str, &str)] = &[
    ("\u{01f916} Tech/ai-llm", "ai"),
    ("\u{01f916} tech/ai-llm", "ai"),
    ("tech/ai-llm", "ai"),
    ("ai-llm", "ai"),
    ("\u{01f916} Tech/rust", "tech"),
    ("\u{01f916} Tech/nixos", "tech"),
    ("\u{01f916} Tech/python", "tech"),
    ("\u{01f916} Tech/tools", "tech"),
    ("\u{01f916} Tech/devops", "tech"),
    ("\u{01f916} Tech/snippets", "tech"),
    ("\u{01f916} tech", "tech"),
    ("\u{01f3c8} Football/research", "football"),
    ("\u{01f3c8} Football", "football"),
    ("\u{270d}\u{fe0f} Writing/craft", "writing"),
    ("\u{270d}\u{fe0f} Writing", "writing"),
    ("\u{01f4bc} Work", "work"),
    ("\u{01f4da} Resources/articles", "resources"),
    ("\u{01f4da} Resources/videos", "resources"),
    ("\u{01f4da} Resources", "resources"),
    ("\u{01f9e0} Knowledge/health", "knowledge"),
    ("\u{01f9e0} Knowledge/learning", "knowledge"),
    ("\u{01f9e0} Knowledge", "knowledge"),
    ("\u{01f3b5} Music", "music"),
    ("\u{01f1ea}\u{01f1f8} Spanish", "spanish"),
    ("\u{2699}\u{fe0f} System", "system"),
    ("\u{01f4e5} Inbox", "inbox"),
    ("Inbox", "inbox"),
];

/// Normalize a domain value to the canonical format.
///
/// Handles:
/// - Old emoji folder paths (e.g. "Tech/ai-llm" -> "ai")
/// - Case normalization (e.g. "AI" -> "ai")
/// - Already-valid values pass through
/// - Unknown values log a warning and pass through lowercased
pub fn normalize_domain(raw: &str) -> String {
    let trimmed = raw.trim();

    // Check exact alias match first (handles emoji paths)
    for &(alias, domain) in DOMAIN_ALIASES {
        if trimmed == alias {
            return domain.to_string();
        }
    }

    // Lowercase and check if it's a valid domain enum value
    let lower = trimmed.to_lowercase();
    if Domain::all().iter().any(|d| d.as_str() == lower) {
        return lower;
    }

    // Try case-insensitive alias match
    let trimmed_lower = trimmed.to_lowercase();
    for &(alias, domain) in DOMAIN_ALIASES {
        if trimmed_lower == alias.to_lowercase() {
            return domain.to_string();
        }
    }

    log::warn!("Unknown domain value '{}', passing through as-is", trimmed);
    lower
}

/// Normalize a text input for use as a content key.
/// Trims whitespace, collapses internal runs to a single space, lowercases.
pub fn normalize_text_input(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

pub fn sanitize_tag(tag: &str) -> String {
    tag.trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

pub fn sanitize_filename(title: &str) -> String {
    let sanitized: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' { c } else { '-' })
        .collect();

    // Collapse consecutive hyphens
    let mut result = String::new();
    let mut prev_hyphen = false;
    for c in sanitized.chars() {
        if c == '-' {
            if !prev_hyphen {
                result.push(c);
            }
            prev_hyphen = true;
        } else {
            result.push(c);
            prev_hyphen = false;
        }
    }

    result.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_domain_valid_passthrough() {
        assert_eq!(normalize_domain("ai"), "ai");
        assert_eq!(normalize_domain("tech"), "tech");
        assert_eq!(normalize_domain("football"), "football");
        assert_eq!(normalize_domain("resources"), "resources");
    }

    #[test]
    fn test_normalize_domain_case_insensitive() {
        assert_eq!(normalize_domain("AI"), "ai");
        assert_eq!(normalize_domain("Tech"), "tech");
        assert_eq!(normalize_domain("FOOTBALL"), "football");
    }

    #[test]
    fn test_normalize_domain_trimming() {
        assert_eq!(normalize_domain("  ai  "), "ai");
    }

    #[test]
    fn test_sanitize_tag_basic() {
        assert_eq!(sanitize_tag("AI/ML"), "ai-ml");
    }

    #[test]
    fn test_sanitize_tag_spaces() {
        assert_eq!(sanitize_tag("Machine Learning"), "machine-learning");
    }

    #[test]
    fn test_sanitize_tag_already_clean() {
        assert_eq!(sanitize_tag("rust"), "rust");
    }

    #[test]
    fn test_sanitize_tag_trim_hyphens() {
        assert_eq!(sanitize_tag("--hello--"), "hello");
    }

    #[test]
    fn test_sanitize_filename_basic() {
        assert_eq!(sanitize_filename("Hello World!"), "hello-world");
    }

    #[test]
    fn test_sanitize_filename_special() {
        assert_eq!(sanitize_filename("Test: A/B \"quotes\""), "test-a-b-quotes");
    }

    #[test]
    fn test_sanitize_filename_collapses_hyphens() {
        assert_eq!(sanitize_filename("a:::b"), "a-b");
    }

    #[test]
    fn test_normalize_text_input_basic() {
        assert_eq!(normalize_text_input("  Definition:  Gregarious  "), "definition: gregarious");
    }

    #[test]
    fn test_normalize_text_input_empty() {
        assert_eq!(normalize_text_input(""), "");
        assert_eq!(normalize_text_input("   "), "");
    }

    #[test]
    fn test_normalize_text_input_tabs_newlines() {
        assert_eq!(normalize_text_input("define:\t\tword\n\n"), "define: word");
    }
}
