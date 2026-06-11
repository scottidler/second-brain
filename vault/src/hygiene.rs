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
    ("\u{01f9e0} Knowledge/health", "life"),
    ("\u{01f9e0} Knowledge/learning", "life"),
    ("\u{01f9e0} Knowledge", "life"),
    ("\u{01f3b5} Music", "music"),
    ("\u{01f1ea}\u{01f1f8} Spanish", "spanish"),
    ("\u{2699}\u{fe0f} System", "system"),
    // No `Inbox -> "inbox"` rows: inbox is a vault LOCATION, not a Domain.
    // `Domain::from_str("inbox")` rejects it, so mapping a domain to "inbox"
    // only produced values the schema then refused.
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

/// Lowercase, strip apostrophes, replace non-alphanumeric with hyphens,
/// collapse consecutive hyphens, and trim leading/trailing hyphens.
fn sanitize_slug(input: &str) -> String {
    let sanitized: String = input
        .to_lowercase()
        .chars()
        .filter(|c| *c != '\'')
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

pub fn sanitize_tag(tag: &str) -> String {
    sanitize_slug(tag.trim())
}

/// Maximum filename stem length (without `.md` extension).
/// Matches cortex naming convention max_length default.
const MAX_FILENAME_LEN: usize = 80;

pub fn sanitize_filename(title: &str) -> String {
    let slug = sanitize_slug(title);

    if slug.len() <= MAX_FILENAME_LEN {
        return slug;
    }

    // Truncate to max length, breaking at a hyphen boundary if possible.
    // sanitize_slug keeps non-ASCII alphanumerics, so snap to a char boundary
    // first - a raw `&slug[..MAX_FILENAME_LEN]` byte cut panicked mid-codepoint.
    let truncated = &slug[..slug.floor_char_boundary(MAX_FILENAME_LEN)];
    if let Some(pos) = truncated.rfind('-')
        && pos > MAX_FILENAME_LEN / 2
    {
        return truncated[..pos].to_string();
    }
    truncated.trim_end_matches('-').to_string()
}

#[cfg(test)]
mod tests;
