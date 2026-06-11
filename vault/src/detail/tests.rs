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
