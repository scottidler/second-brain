use super::*;

#[test]
fn test_parse_plain_url() {
    let result = parse_message("https://youtube.com/watch?v=abc123");
    assert_eq!(
        result,
        Some(ParsedMessage::Url {
            url: "https://youtube.com/watch?v=abc123".to_string(),
            tags: vec![],
            force: false,
            note: None,
        })
    );
}

#[test]
fn test_parse_url_with_surrounding_text() {
    // Phase 8 (ntfy transport capture-note fixture): the prose around the URL
    // becomes the capture note (first-URL token removed, whitespace-collapsed).
    let result = parse_message("Check out this video: https://youtube.com/watch?v=abc123");
    assert_eq!(
        result,
        Some(ParsedMessage::Url {
            url: "https://youtube.com/watch?v=abc123".to_string(),
            tags: vec![],
            force: false,
            note: Some("Check out this video:".to_string()),
        })
    );
}

#[test]
fn test_parse_google_discover_format() {
    let result = parse_message("Article Title\nhttps://example.com/article");
    assert_eq!(
        result,
        Some(ParsedMessage::Url {
            url: "https://example.com/article".to_string(),
            tags: vec![],
            force: false,
            note: Some("Article Title".to_string()),
        })
    );
}

#[test]
fn test_parse_json_body() {
    // `force: true` in the body is IGNORED - ntfy's topic-only auth must
    // not let a topic-guesser trigger a force-overwrite.
    let result = parse_message(r#"{"url": "https://example.com", "tags": ["ai", "rust"], "force": true}"#);
    assert_eq!(
        result,
        Some(ParsedMessage::Url {
            url: "https://example.com".to_string(),
            tags: vec!["ai".to_string(), "rust".to_string()],
            force: false,
            note: None,
        })
    );
}

#[test]
fn test_parse_json_body_with_note() {
    // Phase 8: a JSON ntfy body may carry an explicit `note` capture annotation.
    let result = parse_message(r#"{"url": "https://example.com", "note": "fixes borg's linker"}"#);
    assert_eq!(
        result,
        Some(ParsedMessage::Url {
            url: "https://example.com".to_string(),
            tags: vec![],
            force: false,
            note: Some("fixes borg's linker".to_string()),
        })
    );
}

#[test]
fn test_parse_json_body_minimal() {
    let result = parse_message(r#"{"url": "https://example.com"}"#);
    assert_eq!(
        result,
        Some(ParsedMessage::Url {
            url: "https://example.com".to_string(),
            tags: vec![],
            force: false,
            note: None,
        })
    );
}

#[test]
fn test_parse_empty_message() {
    assert_eq!(parse_message(""), None);
    assert_eq!(parse_message("  "), None);
}

#[test]
fn test_parse_no_url_falls_back_to_text() {
    let result = parse_message("just some text without urls");
    assert_eq!(
        result,
        Some(ParsedMessage::Text("just some text without urls".to_string()))
    );
}

#[test]
fn test_parse_invalid_json_falls_through_to_text() {
    let result = parse_message(r#"{"not_valid_json": }"#);
    assert_eq!(result, Some(ParsedMessage::Text(r#"{"not_valid_json": }"#.to_string())));
}
