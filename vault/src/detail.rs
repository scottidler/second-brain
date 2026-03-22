//! Detail level extraction for notes
//!
//! Parses note bodies by H2 sections and returns content at the requested detail level.
//! This is the shared implementation used by both oracle (MCP) and cortex (daemon).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// How much content to return for a note
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum DetailLevel {
    /// Just frontmatter fields: title, domain, type, status, date, source, tags
    #[default]
    Metadata,
    /// Title + first sentence of summary
    Tldr,
    /// The Summary section content (or first H2 section if no Summary)
    Summary,
    /// Complete note body
    Full,
}

/// A note's body parsed into H2 sections
#[derive(Debug, Serialize)]
pub struct ParsedSections {
    pub heading: Option<String>,
    pub sections: HashMap<String, String>,
    pub preamble: String,
    pub raw: String,
}

/// Parse a note body into H2 sections
pub fn parse_sections(body: &str) -> ParsedSections {
    let mut heading = None;
    let mut sections: HashMap<String, String> = HashMap::new();
    let mut preamble = String::new();
    let mut current_section: Option<String> = None;
    let mut current_content = String::new();

    for line in body.lines() {
        if line.starts_with("# ") && heading.is_none() {
            heading = Some(line.trim_start_matches("# ").to_string());
            continue;
        }

        if let Some(section_name) = line.strip_prefix("## ") {
            if let Some(name) = current_section.take() {
                sections.insert(name, current_content.trim().to_string());
            } else {
                preamble = current_content.trim().to_string();
            }
            current_section = Some(section_name.trim().to_string());
            current_content = String::new();
            continue;
        }

        current_content.push_str(line);
        current_content.push('\n');
    }

    if let Some(name) = current_section.take() {
        sections.insert(name, current_content.trim().to_string());
    } else {
        preamble = current_content.trim().to_string();
    }

    ParsedSections {
        heading,
        sections,
        preamble,
        raw: body.to_string(),
    }
}

/// Extract the first sentence from text
pub fn first_sentence(text: &str) -> String {
    let trimmed = text.trim();
    for (i, c) in trimmed.char_indices() {
        if (c == '.' || c == '!' || c == '?') && i > 0 {
            let rest = &trimmed[i + c.len_utf8()..];
            if rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\n') {
                return trimmed[..=i].to_string();
            }
        }
    }
    trimmed.lines().next().unwrap_or("").to_string()
}

/// Extract the best summary content from a note body using the fallback chain
pub fn extract_summary(body: &str) -> String {
    let sections = parse_sections(body);

    // Prefer ## Summary section
    if let Some(summary) = sections.sections.get("Summary")
        && !summary.is_empty()
    {
        return summary.clone();
    }

    // Fall back to first H2 section
    if let Some(first_content) = sections.sections.values().next()
        && !first_content.is_empty()
    {
        return first_content.clone();
    }

    // Fall back to first 500 chars of body
    let chars: String = body.chars().take(500).collect();
    chars
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_sections() {
        let body =
            "# My Note\n\nSome preamble.\n\n## Summary\n\nThis is the summary.\n\n## Details\n\nMore details here.\n";
        let parsed = parse_sections(body);

        assert_eq!(parsed.heading.as_deref(), Some("My Note"));
        assert_eq!(
            parsed.sections.get("Summary").expect("missing Summary"),
            "This is the summary."
        );
        assert_eq!(
            parsed.sections.get("Details").expect("missing Details"),
            "More details here."
        );
        assert_eq!(parsed.preamble, "Some preamble.");
    }

    #[test]
    fn test_first_sentence() {
        assert_eq!(first_sentence("Hello world. More text."), "Hello world.");
        assert_eq!(first_sentence("Single line"), "Single line");
        assert_eq!(first_sentence("Question? Yes."), "Question?");
    }

    #[test]
    fn test_extract_summary_prefers_summary_section() {
        let body = "# Title\n\n## Summary\n\nThe summary.\n\n## Details\n\nDetails here.\n";
        assert_eq!(extract_summary(body), "The summary.");
    }

    #[test]
    fn test_extract_summary_falls_back_to_first_section() {
        let body = "# Title\n\n## Details\n\nDetails here.\n";
        assert_eq!(extract_summary(body), "Details here.");
    }

    #[test]
    fn test_extract_summary_falls_back_to_body() {
        let body = "# Title\n\nJust some text without sections.\n";
        assert!(extract_summary(body).contains("Just some text"));
    }
}
