#![allow(clippy::unwrap_used)]

use super::*;
use rmcp::model::{CallToolResult, Content};
use serde_json::json;

#[test]
fn outcome_is_failure_protocol_error() {
    let result = CallToolResult::error(vec![Content::text("Provide either 'content' or 'path'")]);
    assert!(outcome_is_failure(&result));
}

#[test]
fn outcome_is_failure_application_not_found() {
    let result = CallToolResult::success(vec![
        Content::json(json!({
            "found": false,
            "kind": "note",
            "path": "notes/missing.md",
            "message": "Note not found",
        }))
        .unwrap(),
    ]);
    assert!(outcome_is_failure(&result));
}

#[test]
fn outcome_is_failure_plain_success_is_not_failure() {
    let result = CallToolResult::success(vec![
        Content::json(json!({
            "count": 3,
            "results": [{"path": "a.md"}, {"path": "b.md"}, {"path": "c.md"}],
        }))
        .unwrap(),
    ]);
    assert!(!outcome_is_failure(&result));
}

#[test]
fn outcome_is_failure_found_true_is_not_failure() {
    let result = CallToolResult::success(vec![
        Content::json(json!({
            "found": true,
            "path": "notes/exists.md",
        }))
        .unwrap(),
    ]);
    assert!(!outcome_is_failure(&result));
}

#[test]
fn outcome_is_failure_text_content_without_json_is_not_failure() {
    let result = CallToolResult::success(vec![Content::text("just some prose with no found key")]);
    assert!(!outcome_is_failure(&result));
}

#[test]
fn outcome_is_failure_empty_content_is_not_failure() {
    let result = CallToolResult::success(vec![]);
    assert!(!outcome_is_failure(&result));
}

#[test]
fn wrap_breaks_on_word_boundaries_within_width() {
    let lines = wrap("the quick brown fox jumps", 10);
    assert_eq!(lines, vec!["the quick", "brown fox", "jumps"]);
    assert!(lines.iter().all(|l| l.len() <= 10));
}

#[test]
fn wrap_overflows_word_longer_than_width_onto_its_own_line() {
    let lines = wrap("a supercalifragilistic b", 8);
    assert_eq!(lines, vec!["a", "supercalifragilistic", "b"]);
}

#[test]
fn wrap_empty_text_yields_no_lines() {
    assert!(wrap("", 40).is_empty());
}

#[test]
fn pad_display_pads_by_display_width_not_char_count() {
    // "日本" is 2 chars but 4 terminal columns.
    assert_eq!(pad_display("日本", 6), "日本  ");
    // "e" + combining acute is 2 chars but 1 column.
    assert_eq!(pad_display("e\u{301}", 3), "e\u{301}  ");
    assert_eq!(pad_display("ai", 5), "ai   ");
}

#[test]
fn pad_display_never_truncates_a_name_wider_than_the_column() {
    assert_eq!(pad_display("software-engineering", 15), "software-engineering");
}

#[test]
fn col_width_is_the_longest_name_when_it_exceeds_the_floor() {
    let names = vec![
        ("ai".to_string(), 1169),
        ("software-engineering".to_string(), 205),
        ("rust".to_string(), 250),
    ];
    assert_eq!(col_width(&names, 15), "software-engineering".len());
}

#[test]
fn col_width_falls_back_to_the_floor_for_short_or_empty_sections() {
    let names = vec![("ai".to_string(), 1), ("llm".to_string(), 2)];
    assert_eq!(col_width(&names, 15), 15);
    assert_eq!(col_width(&[], 10), 10);
}

#[test]
fn col_width_counts_display_columns_for_wide_names() {
    let names = vec![("日本語タグ".to_string(), 1)];
    assert_eq!(col_width(&names, 0), 10);
}

#[derive(clap::Parser)]
struct EvalHarness {
    #[command(flatten)]
    args: EvalArgs,
}

fn parse_eval(argv: &[&str]) -> Result<EvalArgs, clap::Error> {
    use clap::Parser;
    EvalHarness::try_parse_from(std::iter::once("eval").chain(argv.iter().copied())).map(|h| h.args)
}

#[test]
fn eval_target_defaults_to_lift_and_takes_answerability_in_any_case() {
    assert_eq!(parse_eval(&[]).unwrap().target, EvalTarget::Lift);
    assert_eq!(
        parse_eval(&["--target", "Answerability"]).unwrap().target,
        EvalTarget::Answerability
    );
    assert!(parse_eval(&["--target", "nope"]).is_err());
}

#[test]
fn mine_queries_stands_alone_but_a_default_target_does_not_conflict() {
    let a = parse_eval(&["--mine-queries", "/tmp/q.yml"]).unwrap();
    assert_eq!(a.mine_queries, Some(PathBuf::from("/tmp/q.yml")));
    assert!(parse_eval(&["--mine-queries", "/tmp/q.yml", "--target", "answerability"]).is_err());
    assert!(parse_eval(&["--mine-queries", "/tmp/q.yml", "--emit-calibration", "/tmp/c.yml"]).is_err());
    assert!(parse_eval(&["--target", "answerability", "--emit-calibration", "/tmp/c.yml"]).is_err());
}

#[test]
fn the_staged_source_reader_maps_borgs_staging_outcomes() {
    use oracle::eval::answerability::{SourceReader, SourceText};
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("tr-1");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("envelope.yml"), "kind: article-url\n").unwrap();
    std::fs::write(
        dir.join("distilled.yml"),
        "summary: s\nclaims: []\nmeta:\n  extractor: x\n  model: m\n  produced-at: \"2026-07-05T00:00:00Z\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("transcript.md"), "the verbatim source").unwrap();
    std::fs::create_dir_all(tmp.path().join("tr-empty")).unwrap();

    let reader = StagedSourceReader {
        root: tmp.path().to_path_buf(),
    };
    assert_eq!(reader.read("tr-1"), SourceText::Found("the verbatim source".into()));
    assert_eq!(reader.read("tr-swept"), SourceText::Expired);
    assert!(matches!(reader.read("tr-empty"), SourceText::Unreadable(_)));
}
