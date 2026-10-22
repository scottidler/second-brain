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

// ---- keep-filter + tally -------------------------------------------

use crate::config::{ContentFilterConfig, SlideClass};

fn filter_keeping(keep: Vec<SlideCategory>, min_confidence: f32) -> ContentFilterConfig {
    ContentFilterConfig {
        enabled: true,
        keep,
        model: String::new(),
        max_vision_concurrency: 4,
        min_confidence,
    }
}

fn ok(category: SlideCategory, confidence: f32) -> std::result::Result<SlideClass, ClassifyError> {
    Ok(SlideClass { category, confidence })
}

#[test]
fn test_keep_outcome_keep_in_category_above_confidence() {
    let filter = filter_keeping(vec![SlideCategory::Code], 0.6);
    let r = ok(SlideCategory::Code, 0.9);
    assert_eq!(keep_outcome(&r, &filter), KeepOutcome::Keep);
}

#[test]
fn test_keep_outcome_drops_below_confidence() {
    let filter = filter_keeping(vec![SlideCategory::Code], 0.6);
    let r = ok(SlideCategory::Code, 0.5);
    assert_eq!(keep_outcome(&r, &filter), KeepOutcome::DroppedLowConfidence);
}

#[test]
fn test_keep_outcome_drops_category_not_in_keep() {
    let filter = filter_keeping(vec![SlideCategory::ArchitectureDiagram], 0.6);
    let r = ok(SlideCategory::TalkingHead, 0.99);
    assert_eq!(keep_outcome(&r, &filter), KeepOutcome::DroppedNotInKeep);
}

#[test]
fn test_keep_outcome_confidence_at_threshold_is_kept() {
    let filter = filter_keeping(vec![SlideCategory::Code], 0.6);
    let r = ok(SlideCategory::Code, 0.6);
    assert_eq!(keep_outcome(&r, &filter), KeepOutcome::Keep);
}

#[test]
fn test_keep_outcome_api_error_is_degradation_signal() {
    let filter = filter_keeping(vec![SlideCategory::Code], 0.6);
    let r: std::result::Result<SlideClass, ClassifyError> = Err(ClassifyError::Api(eyre!("503")));
    assert_eq!(keep_outcome(&r, &filter), KeepOutcome::DroppedApiError);
    let r: std::result::Result<SlideClass, ClassifyError> = Err(ClassifyError::Read(eyre!("no file")));
    assert_eq!(keep_outcome(&r, &filter), KeepOutcome::DroppedApiError);
    let r: std::result::Result<SlideClass, ClassifyError> = Err(ClassifyError::Join(eyre!("panic")));
    assert_eq!(keep_outcome(&r, &filter), KeepOutcome::DroppedApiError);
}

#[test]
fn test_keep_outcome_parse_error_is_distinct() {
    let filter = filter_keeping(vec![SlideCategory::Code], 0.6);
    let r: std::result::Result<SlideClass, ClassifyError> = Err(ClassifyError::Parse(eyre!("off-format")));
    assert_eq!(keep_outcome(&r, &filter), KeepOutcome::DroppedParseError);
}

#[test]
fn test_tally_records_each_bucket() {
    let filter = filter_keeping(vec![SlideCategory::Code, SlideCategory::ArchitectureDiagram], 0.6);
    let results: Vec<std::result::Result<SlideClass, ClassifyError>> = vec![
        ok(SlideCategory::Code, 0.9),                // keep
        ok(SlideCategory::ArchitectureDiagram, 0.7), // keep
        ok(SlideCategory::Code, 0.3),                // low confidence
        ok(SlideCategory::TalkingHead, 0.99),        // not in keep
        Err(ClassifyError::Api(eyre!("boom"))),      // api error
        Err(ClassifyError::Parse(eyre!("garbage"))), // parse error
    ];
    let mut tally = ClassifyTally::default();
    for r in &results {
        tally.record(keep_outcome(r, &filter));
    }
    assert_eq!(tally.classified, 6);
    assert_eq!(tally.kept, 2);
    assert_eq!(tally.dropped_low_confidence, 1);
    assert_eq!(tally.dropped_not_in_keep, 1);
    assert_eq!(tally.dropped_api_error, 1);
    assert_eq!(tally.dropped_parse_error, 1);
}

#[test]
fn test_tally_all_other_keeps_nothing() {
    let filter = filter_keeping(vec![SlideCategory::ArchitectureDiagram], 0.6);
    let results: Vec<std::result::Result<SlideClass, ClassifyError>> = vec![
        ok(SlideCategory::TalkingHead, 0.9),
        ok(SlideCategory::TitleCard, 0.9),
        ok(SlideCategory::Chart, 0.9),
    ];
    let mut tally = ClassifyTally::default();
    for r in &results {
        tally.record(keep_outcome(r, &filter));
    }
    assert_eq!(tally.classified, 3);
    assert_eq!(tally.kept, 0);
    assert_eq!(tally.dropped_not_in_keep, 3);
}

#[test]
fn test_strip_key_matches_case_insensitively() {
    assert_eq!(strip_key("category: foo", "CATEGORY"), Some(" foo"));
    assert_eq!(strip_key("CONFIDENCE:0.5", "CONFIDENCE"), Some("0.5"));
    assert_eq!(strip_key("notakey: x", "CATEGORY"), None);
    assert_eq!(strip_key("no-colon-here", "CATEGORY"), None);
}

// ---------------------------------------------------------------------------
// Figure classification (docs/design/2026-10-22-capture-all-diagrams.md)
// ---------------------------------------------------------------------------

#[test]
fn keep_outcome_for_applies_the_figure_keep_list_and_threshold() {
    let keep = crate::config::FigureFilterConfig::default().keep;
    assert_eq!(
        keep_outcome_for(&ok(SlideCategory::Chart, 0.6), &keep, 0.6),
        KeepOutcome::Keep
    );
    assert_eq!(
        keep_outcome_for(&ok(SlideCategory::Chart, 0.59), &keep, 0.6),
        KeepOutcome::DroppedLowConfidence
    );
    assert_eq!(
        keep_outcome_for(&ok(SlideCategory::TitleCard, 0.99), &keep, 0.6),
        KeepOutcome::DroppedNotInKeep
    );
    let api: std::result::Result<SlideClass, ClassifyError> = Err(ClassifyError::Api(eyre!("boom")));
    assert_eq!(keep_outcome_for(&api, &keep, 0.6), KeepOutcome::DroppedApiError);
}

/// A one-route vision stub answering every `/v1/messages` with `(code, text)`
/// and recording the prompt it was sent.
async fn vision_stub(code: u16, text: &'static str) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use axum::response::IntoResponse;
    let prompts = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = prompts.clone();
    let app = axum::Router::new().route(
        "/v1/messages",
        axum::routing::post(move |body: String| {
            let seen = seen.clone();
            async move {
                let request: serde_json::Value = serde_json::from_str(&body).expect("body");
                let prompt = request["messages"][0]["content"][1]["text"].as_str().unwrap_or("");
                seen.lock().expect("prompts").push(prompt.to_string());
                let reply = serde_json::json!({"content": [{"type": "text", "text": text}]});
                let code = axum::http::StatusCode::from_u16(code).expect("status");
                (code, [("content-type", "application/json")], reply.to_string()).into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base = format!("http://127.0.0.1:{}", listener.local_addr().expect("addr").port());
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    (base, prompts)
}

fn llm_with_key(dir: &Path) -> LlmConfig {
    let key = dir.join("anthropic.key");
    std::fs::write(&key, "stub-key").expect("key");
    LlmConfig {
        api_key: key.to_string_lossy().to_string(),
        ..LlmConfig::default()
    }
}

#[tokio::test]
async fn classify_figure_sends_the_figure_prompt_and_parses_the_reply() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (base, prompts) = vision_stub(200, "CATEGORY: flowchart\nCONFIDENCE: 0.8").await;
    let filter = crate::config::FigureFilterConfig::default();

    let class = crate::ocr::bases::scope(
        base,
        classify_figure(b"jpeg", "image/jpeg", &filter, &llm_with_key(dir.path())),
    )
    .await
    .expect("classified");

    assert_eq!(class.category, SlideCategory::Flowchart);
    assert!((class.confidence - 0.8).abs() < f32::EPSILON);
    assert_eq!(prompts.lock().expect("prompts").as_slice(), [FIGURE_TAXONOMY_PROMPT]);
}

#[tokio::test]
async fn classify_figure_fails_closed_on_api_error_and_off_format_reply() {
    let dir = tempfile::tempdir().expect("tempdir");
    let filter = crate::config::FigureFilterConfig::default();
    let llm = llm_with_key(dir.path());

    let (base, _) = vision_stub(500, "boom").await;
    let err = crate::ocr::bases::scope(base, classify_figure(b"jpeg", "image/jpeg", &filter, &llm))
        .await
        .expect_err("500 fails");
    assert!(matches!(err, ClassifyError::Api(_)), "{err}");

    let (base, _) = vision_stub(200, "a lovely diagram").await;
    let err = crate::ocr::bases::scope(base, classify_figure(b"jpeg", "image/jpeg", &filter, &llm))
        .await
        .expect_err("off-format fails");
    assert!(matches!(err, ClassifyError::Parse(_)), "{err}");
}
