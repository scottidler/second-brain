use super::*;
use crate::config::SlideCategory;

#[test]
fn test_parse_well_formed() {
    let raw = "CATEGORY: architecture-diagram\nCONFIDENCE: 0.92";
    let class = parse_classification(raw).expect("well-formed reply parses");
    assert_eq!(class.category, SlideCategory::ArchitectureDiagram);
    assert!((class.confidence - 0.92).abs() < f32::EPSILON);
}

#[test]
fn test_parse_case_insensitive_category() {
    let raw = "CATEGORY: Architecture-Diagram\nCONFIDENCE: 0.5";
    let class = parse_classification(raw).expect("case-insensitive category parses");
    assert_eq!(class.category, SlideCategory::ArchitectureDiagram);
}

#[test]
fn test_parse_tolerates_surrounding_whitespace_and_decoration() {
    let raw = "  **CATEGORY:** code  \n  - CONFIDENCE: 0.75  ";
    let class = parse_classification(raw).expect("decorated reply parses");
    assert_eq!(class.category, SlideCategory::Code);
    assert!((class.confidence - 0.75).abs() < f32::EPSILON);
}

#[test]
fn test_parse_extra_prose_around_fields_still_parses() {
    let raw = "Here is my answer.\nCATEGORY: terminal\nCONFIDENCE: 0.6\nThanks!";
    let class = parse_classification(raw).expect("fields embedded in prose parse");
    assert_eq!(class.category, SlideCategory::Terminal);
}

#[test]
fn test_parse_confidence_boundaries() {
    let zero = parse_classification("CATEGORY: other\nCONFIDENCE: 0.0").expect("0.0 ok");
    assert_eq!(zero.confidence, 0.0);
    let one = parse_classification("CATEGORY: other\nCONFIDENCE: 1.0").expect("1.0 ok");
    assert_eq!(one.confidence, 1.0);
}

#[test]
fn test_parse_unknown_category_fails_closed() {
    let raw = "CATEGORY: spaceship\nCONFIDENCE: 0.99";
    assert!(parse_classification(raw).is_err(), "unknown category must Err");
}

#[test]
fn test_parse_missing_category_fails_closed() {
    let raw = "CONFIDENCE: 0.8";
    assert!(parse_classification(raw).is_err(), "missing CATEGORY must Err");
}

#[test]
fn test_parse_missing_confidence_fails_closed() {
    let raw = "CATEGORY: chart";
    assert!(parse_classification(raw).is_err(), "missing CONFIDENCE must Err");
}

#[test]
fn test_parse_nonnumeric_confidence_fails_closed() {
    let raw = "CATEGORY: chart\nCONFIDENCE: very-high";
    assert!(parse_classification(raw).is_err(), "non-numeric CONFIDENCE must Err");
}

#[test]
fn test_parse_confidence_out_of_range_fails_closed() {
    assert!(
        parse_classification("CATEGORY: chart\nCONFIDENCE: 1.5").is_err(),
        ">1.0 confidence must Err"
    );
    assert!(
        parse_classification("CATEGORY: chart\nCONFIDENCE: -0.1").is_err(),
        "negative confidence must Err"
    );
}

#[test]
fn test_parse_empty_reply_fails_closed() {
    assert!(parse_classification("").is_err(), "empty reply must Err");
}

#[test]
fn test_parse_garbage_reply_fails_closed() {
    let raw = "I cannot determine what this image shows.";
    assert!(parse_classification(raw).is_err(), "free-text refusal must Err");
}

#[test]
fn test_parse_ambiguous_conflicting_categories_fails_closed() {
    // Two CATEGORY lines naming different categories is ambiguous -> fail closed.
    let raw = "CATEGORY: architecture-diagram\nCATEGORY: webpage\nCONFIDENCE: 0.7";
    assert!(
        parse_classification(raw).is_err(),
        "conflicting CATEGORY lines must Err"
    );
}

#[test]
fn test_parse_repeated_identical_category_is_not_ambiguous() {
    // The same category twice is not a conflict; last CONFIDENCE wins.
    let raw = "CATEGORY: code\nCATEGORY: code\nCONFIDENCE: 0.8";
    let class = parse_classification(raw).expect("repeated identical category parses");
    assert_eq!(class.category, SlideCategory::Code);
}

#[test]
fn test_preview_truncates_and_flattens() {
    let long = format!("line one\nline two {}", "x".repeat(200));
    let p = preview(&long);
    assert!(!p.contains('\n'), "preview flattens newlines");
    assert!(p.chars().count() <= 80, "preview is bounded");
}

#[test]
fn test_strip_key_matches_case_insensitively() {
    assert_eq!(strip_key("category: foo", "CATEGORY"), Some(" foo"));
    assert_eq!(strip_key("CONFIDENCE:0.5", "CONFIDENCE"), Some("0.5"));
    assert_eq!(strip_key("notakey: x", "CATEGORY"), None);
    assert_eq!(strip_key("no-colon-here", "CATEGORY"), None);
}
