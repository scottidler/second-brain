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
    /// Section names in document order (first-seen), so "the first H2 section"
    /// is deterministic. `sections` (a HashMap) has no stable iteration order.
    pub order: Vec<String>,
    pub preamble: String,
    pub raw: String,
}

/// Close the in-progress section: store its content (merging duplicates by
/// name so a second `## Summary` doesn't silently clobber the first) and record
/// first-seen order. With no open section the content is the preamble.
fn close_section(
    sections: &mut HashMap<String, String>,
    order: &mut Vec<String>,
    current_section: &mut Option<String>,
    current_content: &str,
    preamble: &mut String,
) {
    let content = current_content.trim().to_string();
    match current_section.take() {
        Some(name) => {
            if let Some(existing) = sections.get_mut(&name) {
                // Duplicate H2 name: append rather than overwrite.
                existing.push_str("\n\n");
                existing.push_str(&content);
            } else {
                order.push(name.clone());
                sections.insert(name, content);
            }
        }
        None => *preamble = content,
    }
}

/// Parse a note body into H2 sections. Skips `# `/`## ` markers inside fenced
/// code blocks (``` / ~~~), tracks document order, and merges duplicate H2 names.
pub fn parse_sections(body: &str) -> ParsedSections {
    let mut heading = None;
    let mut sections: HashMap<String, String> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut preamble = String::new();
    let mut current_section: Option<String> = None;
    let mut current_content = String::new();
    let mut in_fence = false;

    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
            current_content.push_str(line);
            current_content.push('\n');
            continue;
        }

        if !in_fence && line.starts_with("# ") && heading.is_none() {
            heading = Some(line.trim_start_matches("# ").to_string());
            continue;
        }

        if !in_fence && let Some(section_name) = line.strip_prefix("## ") {
            close_section(
                &mut sections,
                &mut order,
                &mut current_section,
                &current_content,
                &mut preamble,
            );
            current_section = Some(section_name.trim().to_string());
            current_content = String::new();
            continue;
        }

        current_content.push_str(line);
        current_content.push('\n');
    }

    close_section(
        &mut sections,
        &mut order,
        &mut current_section,
        &current_content,
        &mut preamble,
    );

    ParsedSections {
        heading,
        sections,
        order,
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

    // Fall back to the FIRST H2 section in document order (deterministic;
    // iterating the HashMap's values was nondeterministic → summary/embedding
    // churn across reindexes).
    if let Some(first_content) = sections.order.first().and_then(|name| sections.sections.get(name))
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

    #[test]
    fn first_h2_fallback_is_document_order_not_hashmap_order() {
        // No Summary section: the fallback must be the FIRST H2 in document
        // order, deterministically, regardless of HashMap iteration order.
        let body = "## Alpha\n\nalpha content\n\n## Beta\n\nbeta content\n\n## Gamma\n\ngamma content\n";
        assert_eq!(extract_summary(body), "alpha content");
        let parsed = parse_sections(body);
        assert_eq!(parsed.order, vec!["Alpha", "Beta", "Gamma"]);
    }

    #[test]
    fn h2_inside_fenced_code_block_is_not_a_section() {
        // A `## ` line inside a fenced code block is code, not a heading.
        let body = "## Real\n\nbefore\n\n```\n## Not A Heading\n```\n\nafter\n";
        let parsed = parse_sections(body);
        assert_eq!(parsed.order, vec!["Real"]);
        assert!(parsed.sections.get("Real").expect("Real").contains("## Not A Heading"));
        assert!(!parsed.sections.contains_key("Not A Heading"));
    }

    #[test]
    fn duplicate_h2_names_merge_not_clobber() {
        let body = "## Notes\n\nfirst\n\n## Notes\n\nsecond\n";
        let parsed = parse_sections(body);
        assert_eq!(parsed.order, vec!["Notes"]);
        let notes = parsed.sections.get("Notes").expect("Notes");
        assert!(notes.contains("first") && notes.contains("second"), "got {notes:?}");
    }
}
