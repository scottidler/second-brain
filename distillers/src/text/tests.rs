use super::*;

#[test]
fn test_demote_headings_h2_to_h4() {
    let input = "## Foo\nbody text\n";
    let out = demote_headings(input, 2);
    assert_eq!(out, "#### Foo\nbody text\n");
}

#[test]
fn test_demote_headings_mixed_levels() {
    let input = "# H1\n## H2\n### H3\nbody";
    let out = demote_headings(input, 2);
    assert_eq!(out, "### H1\n#### H2\n##### H3\nbody");
}

#[test]
fn test_demote_headings_zero_levels_is_noop() {
    let input = "## Foo\nbody";
    assert_eq!(demote_headings(input, 0), input);
}

#[test]
fn test_demote_headings_preserves_non_heading_lines() {
    // Hashtag-style (no space after #) and trailing-# lines stay as-is.
    let input = "#nospace\n#### \nplain prose\n##notspace\n#";
    let out = demote_headings(input, 3);
    assert_eq!(out, input);
}

#[test]
fn test_demote_headings_preserves_empty_lines_and_terminal_newline() {
    let input = "## A\n\n## B\n";
    let out = demote_headings(input, 1);
    assert_eq!(out, "### A\n\n### B\n");
}

#[test]
fn test_demote_headings_with_indented_heading() {
    // Indented `   ## Foo` (still a heading per CommonMark) gets demoted in place.
    let input = "   ## Indented\nbody";
    let out = demote_headings(input, 2);
    assert_eq!(out, "   #### Indented\nbody");
}

#[test]
fn test_demote_headings_handles_legacy_note_body_shape() {
    // Realistic pre-L2 video-note body: an H1 title, embedded iframe, then
    // legacy ## headings. After demotion none of them collide with the new
    // ## Transcript wrapper that will be added around this text.
    let input = "\
# 10 CLI Tools

<iframe ...></iframe>

## Summary

Legacy tldr text.

## Description

Channel description.";
    let out = demote_headings(input, 2);
    assert!(out.starts_with("### 10 CLI Tools"));
    assert!(out.contains("\n#### Summary\n"));
    assert!(out.contains("\n#### Description\n"));
    // The iframe and prose stay untouched.
    assert!(out.contains("<iframe ...></iframe>"));
    assert!(out.contains("Legacy tldr text."));
}

#[test]
fn test_demote_headings_empty_input() {
    assert_eq!(demote_headings("", 3), "");
}
