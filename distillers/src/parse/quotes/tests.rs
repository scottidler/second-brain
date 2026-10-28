use super::super::parse_pattern_yaml;
use super::repair_double_quoted;
use serde::Deserialize;

#[derive(Debug, Deserialize, PartialEq)]
struct Out {
    summary: String,
    tldr: String,
    #[serde(default, rename = "key-ideas")]
    key_ideas: Vec<String>,
}

// cl-1f55a006 (HN 40857174): a bare `"` inside the double-quoted summary.
#[test]
fn a_bare_inner_quote_is_escaped() {
    let raw = "summary: \"Commenters question whether the \"knowledge graph\" framing is substantive.\"\ntldr: \"GraphRAG shipped.\"";
    let out: Out = parse_pattern_yaml(raw).expect("repaired");
    assert_eq!(
        out.summary,
        "Commenters question whether the \"knowledge graph\" framing is substantive."
    );
    assert_eq!(out.tldr, "GraphRAG shipped.");
}

// cl-683d8eb1 (arxiv 2502.14802): the summary's closing quote is missing.
#[test]
fn an_unclosed_quote_before_the_next_key_is_closed() {
    let raw = "summary: \"HippoRAG 2 improves associative memory by 7%.\ntldr: \"PPR plus passages.\"";
    let out: Out = parse_pattern_yaml(raw).expect("repaired");
    assert_eq!(out.summary, "HippoRAG 2 improves associative memory by 7%.");
}

// cl-92830229: a JSON-style comma after the closing quote.
#[test]
fn a_trailing_comma_after_the_closing_quote_is_dropped() {
    let raw = "summary: \"Workloads remain unaffected.\", \ntldr: \"Upgrade in place.\"";
    let out: Out = parse_pattern_yaml(raw).expect("repaired");
    assert_eq!(out.summary, "Workloads remain unaffected.");
}

#[test]
fn list_items_are_repaired_too() {
    let raw = "summary: \"s\"\ntldr: \"t\"\nkey-ideas:\n  - \"**Live feedback** - small corrections (\"a little bit left\") each snap\"\n  - \"second\"";
    let out: Out = parse_pattern_yaml(raw).expect("repaired");
    assert_eq!(
        out.key_ideas[0],
        "**Live feedback** - small corrections (\"a little bit left\") each snap"
    );
}

#[test]
fn an_unclosed_quote_at_the_end_of_the_document_is_left_to_fail() {
    // A truncated output: closing the quote would hide the lost tail.
    let raw = "summary: \"s\"\ntldr: \"A comprehensive front-end web fram";
    assert!(parse_pattern_yaml::<Out>(raw).is_err());
    assert_eq!(repair_double_quoted(raw).1, 0);
}

#[test]
fn a_whole_document_on_one_line_is_left_to_fail() {
    let raw = "summary: \"s.\" tldr: \"t.\" key-ideas: - \"a\"";
    assert!(parse_pattern_yaml::<Out>(raw).is_err());
}

#[test]
fn block_scalar_content_is_never_touched() {
    let raw = "summary: |\n  - \"quoted \"inside\" a block\n  tldr: \"not a key\ntldr: \"t\"";
    let (text, repaired) = repair_double_quoted(raw);
    assert_eq!(repaired, 0);
    assert_eq!(text, raw);
}

#[test]
fn a_legal_multi_line_quoted_scalar_is_left_alone() {
    // The continuation is indented deeper, so the scalar has not ended.
    let raw = "summary: \"first half\n  second half\"\ntldr: \"t\"";
    assert_eq!(repair_double_quoted(raw).1, 0);
    let out: Out = parse_pattern_yaml(raw).expect("valid as written");
    assert_eq!(out.summary, "first half second half");
}

#[test]
fn already_escaped_quotes_are_not_double_escaped() {
    let raw = "summary: \"the \\\"graph\\\" view\"\ntldr: \"t\"";
    assert_eq!(repair_double_quoted(raw).1, 0);
}
