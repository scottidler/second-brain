use super::*;

#[test]
fn test_missing_api_key_returns_error() {
    // Use an env var name that definitely doesn't exist
    let result = complete("system", "user", "model", 100, 10, "NONEXISTENT_TEST_KEY_12345");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("NONEXISTENT_TEST_KEY_12345"),
        "error should mention the missing env var: {err}"
    );
}

#[test]
fn test_extract_text_skips_leading_thinking_block() {
    let parsed = serde_json::json!({
        "stop_reason": "end_turn",
        "content": [
            {"type": "thinking", "thinking": "", "signature": "sig"},
            {"type": "text", "text": "the digest"}
        ]
    });
    assert_eq!(extract_text(&parsed).unwrap(), "the digest");
}

#[test]
fn test_extract_text_plain_text_response() {
    let parsed = serde_json::json!({
        "stop_reason": "end_turn",
        "content": [{"type": "text", "text": "the digest"}]
    });
    assert_eq!(extract_text(&parsed).unwrap(), "the digest");
}

#[test]
fn test_extract_text_max_tokens_on_thinking_only_is_error() {
    // The 2026-10 digest failure: thinking ate the whole budget.
    let parsed = serde_json::json!({
        "stop_reason": "max_tokens",
        "content": [{"type": "thinking", "thinking": "", "signature": "sig"}]
    });
    let err = extract_text(&parsed).unwrap_err().to_string();
    assert!(err.contains("stop_reason=max_tokens"), "{err}");
    assert!(err.contains("thinking"), "{err}");
}

#[test]
fn test_extract_text_max_tokens_with_partial_text_is_error() {
    let parsed = serde_json::json!({
        "stop_reason": "max_tokens",
        "content": [
            {"type": "thinking", "thinking": "", "signature": "sig"},
            {"type": "text", "text": "cut off mid-sen"}
        ]
    });
    assert!(extract_text(&parsed).is_err());
}

#[test]
fn test_extract_text_refusal_is_error() {
    let parsed = serde_json::json!({"stop_reason": "refusal", "content": []});
    let err = extract_text(&parsed).unwrap_err().to_string();
    assert!(err.contains("stop_reason=refusal"), "{err}");
}

#[test]
fn test_extract_text_end_turn_without_text_is_error() {
    let parsed = serde_json::json!({
        "stop_reason": "end_turn",
        "content": [{"type": "thinking", "thinking": "", "signature": "sig"}]
    });
    assert!(extract_text(&parsed).is_err());
}

#[test]
fn test_intel_default_output_budget_leaves_room_for_thinking() {
    assert!(crate::config::IntelConfig::default().max_output_tokens >= 16000);
}

#[test]
fn test_truncate_input_short() {
    let input = "hello world";
    assert_eq!(truncate_input(input, 50000), "hello world");
}

#[test]
fn test_truncate_input_long() {
    let input = "a".repeat(300_000);
    let result = truncate_input(&input, 50000);
    assert!(result.len() <= 200_000);
}
