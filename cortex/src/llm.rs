//! Lightweight Anthropic Messages API client.
//!
//! Provides a single `complete()` function for one-shot completions.
//! Used by the intel module for daily digest synthesis.

use eyre::{Context, Result};
use serde_json::json;
use std::time::Duration;

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";

/// Send a one-shot completion request to the Anthropic Messages API.
///
/// Reads the API key from the environment variable named by `api_key_env`.
/// Returns the text content of the first response block.
pub fn complete(
    system: &str,
    user: &str,
    model: &str,
    max_tokens: u32,
    timeout_secs: u64,
    api_key_env: &str,
) -> Result<String> {
    let api_key =
        std::env::var(api_key_env).with_context(|| format!("environment variable {api_key_env} is not set"))?;

    let body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "system": system,
        "messages": [
            {
                "role": "user",
                "content": user
            }
        ]
    });

    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(timeout_secs)))
        .build()
        .new_agent();

    let mut response = agent
        .post(API_URL)
        .header("x-api-key", &api_key)
        .header("anthropic-version", API_VERSION)
        .header("content-type", "application/json")
        .send_json(&body)
        .context("anthropic API request failed")?;

    let response_text = response
        .body_mut()
        .read_to_string()
        .context("failed to read API response body")?;

    let parsed: serde_json::Value =
        serde_json::from_str(&response_text).context("failed to parse API response JSON")?;

    // Extract text from content[0].text
    parsed["content"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|block| block["text"].as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| eyre::eyre!("unexpected API response structure: no content[0].text"))
}

/// Truncate input text to approximately max_tokens (estimated at ~4 chars per token).
pub fn truncate_input(input: &str, max_tokens: usize) -> &str {
    let max_chars = max_tokens * 4;
    if input.len() <= max_chars {
        input
    } else {
        let end = input.floor_char_boundary(max_chars);
        &input[..end]
    }
}

#[cfg(test)]
mod tests {
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
}
