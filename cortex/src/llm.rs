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
    log::debug!(
        "llm::complete: model={model} max_tokens={max_tokens} timeout_secs={timeout_secs} system_len={} user_len={}",
        system.len(),
        user.len()
    );
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
        .http_status_as_error(false)
        .build()
        .new_agent();

    let mut response = agent
        .post(API_URL)
        .header("x-api-key", &api_key)
        .header("anthropic-version", API_VERSION)
        .header("content-type", "application/json")
        .send_json(&body)
        .context("anthropic API request failed")?;

    let status = response.status().as_u16();
    let response_text = response
        .body_mut()
        .read_to_string()
        .context("failed to read API response body")?;

    if status != 200 {
        return Err(eyre::eyre!("anthropic API returned {status}: {response_text}"));
    }

    let parsed: serde_json::Value =
        serde_json::from_str(&response_text).context("failed to parse API response JSON")?;

    extract_text(&parsed)
}

/// Pull the answer text out of a Messages API response.
///
/// Models with adaptive thinking on by default (claude-sonnet-5 and later)
/// put a `thinking` block before the `text` block, so `content[0]` is not the
/// answer. Thinking also spends `max_tokens`: a response that stops on
/// `max_tokens` is either missing its text or truncated, and is an error
/// rather than a half-written digest.
fn extract_text(parsed: &serde_json::Value) -> Result<String> {
    let stop_reason = parsed["stop_reason"].as_str().unwrap_or("<none>");
    let blocks = parsed["content"].as_array().map(Vec::as_slice).unwrap_or_default();
    let block_types: Vec<&str> = blocks
        .iter()
        .map(|b| b["type"].as_str().unwrap_or("<untyped>"))
        .collect();
    log::debug!("llm::extract_text: stop_reason={stop_reason} content types={block_types:?}");

    if stop_reason != "end_turn" {
        return Err(eyre::eyre!(
            "API response did not finish (stop_reason={stop_reason}, content types={block_types:?}); raise intel.max-output-tokens if this is max_tokens"
        ));
    }

    let text: String = blocks
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect();
    if text.trim().is_empty() {
        return Err(eyre::eyre!(
            "API response has no text (stop_reason={stop_reason}, content types={block_types:?})"
        ));
    }
    Ok(text)
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
mod tests;
