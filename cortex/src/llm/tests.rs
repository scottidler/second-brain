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
