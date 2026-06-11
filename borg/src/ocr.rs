use base64::Engine;
use eyre::{Context, Result};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::config::{self, LlmConfig, VisionConfig};

/// HTTP timeout for the Claude Vision API call. The dispatch-level pipeline
/// hard timeout cancels the future, but the reqwest client had NO timeout of
/// its own, so a stalled connection could hang up to the hard timeout with no
/// tighter bound. This is the per-call backstop, matching the rest of borg's
/// external-call timeout discipline.
const VISION_HTTP_TIMEOUT_SECS: u64 = 120;

/// Result of vision-based image description.
pub struct VisionResult {
    pub description: String,
    pub suggested_title: String,
    pub suggested_tags: Vec<String>,
    pub extracted_text: String,
}

/// Extract text from an image using tesseract CLI.
/// Returns empty string if tesseract is not available or fails.
/// Synchronous (called from both rayon `par_iter` and tokio `spawn_blocking`),
/// bounded by `timeout_secs` via a poll-based internal timeout that kills the
/// child process on elapsed.
pub fn ocr_extract(image_path: &Path, timeout_secs: u64) -> Result<String> {
    let mut child = Command::new("tesseract")
        .args([
            image_path.to_str().unwrap_or_default(),
            "stdout",
            "--oem",
            "3",
            "--psm",
            "3",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("Failed to spawn tesseract")?;

    let timeout = Duration::from_secs(timeout_secs);
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    log::warn!("tesseract timed out after {timeout_secs}s");
                    return Ok(String::new());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(eyre::eyre!("Failed to wait for tesseract: {e}")),
        }
    }

    let output = child.wait_with_output().context("Failed to collect tesseract output")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        log::warn!("tesseract failed: {stderr}");
        return Ok(String::new());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Extract text and describe an image using the Claude Vision API directly.
///
/// Sends image bytes as base64 to the Anthropic Messages API.
/// Returns structured results or error if API key unavailable or call fails.
pub async fn vision_extract(
    image_data: &[u8],
    mime_type: &str,
    vision_config: &VisionConfig,
    llm_config: &LlmConfig,
) -> Result<VisionResult> {
    let api_key = config::resolve_secret(&llm_config.api_key)?;

    let b64 = base64::engine::general_purpose::STANDARD.encode(image_data);

    let model = if vision_config.model.is_empty() {
        &llm_config.model
    } else {
        &vision_config.model
    };

    let body = serde_json::json!({
        "model": model,
        "max_tokens": 1024,
        "messages": [{
            "role": "user",
            "content": [
                {
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": mime_type,
                        "data": b64
                    }
                },
                {
                    "type": "text",
                    "text": "Extract ALL text visible in this image and describe what you see.\n\nRespond in this exact format:\nTEXT: <all visible text, preserving layout>\nDESCRIPTION: <2-3 sentence description>\nTITLE: <3-8 word title>\nTAGS: <tag1>, <tag2>, <tag3>"
                }
            ]
        }]
    });

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(VISION_HTTP_TIMEOUT_SECS))
        .build()
        .context("Failed to build vision HTTP client")?;
    let resp = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", &api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .context("Failed to send vision API request")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        eyre::bail!("Vision API returned {status}: {text}");
    }

    let json: serde_json::Value = resp.json().await.context("Failed to parse vision API response")?;

    let output = json["content"]
        .as_array()
        .and_then(|arr| arr.iter().find(|b| b["type"] == "text"))
        .and_then(|b| b["text"].as_str())
        .unwrap_or("")
        .to_string();

    Ok(parse_vision_response(&output))
}

/// Parse the structured text response from the vision API into a VisionResult.
pub fn parse_vision_response(output: &str) -> VisionResult {
    let mut extracted_text = String::new();
    let mut description = String::new();
    let mut suggested_title = String::new();
    let mut suggested_tags = Vec::new();

    // Track which section we're in for multi-line TEXT blocks
    let mut in_text_section = false;

    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(text) = trimmed.strip_prefix("TEXT:") {
            extracted_text = text.trim().to_string();
            in_text_section = true;
        } else if let Some(desc) = trimmed.strip_prefix("DESCRIPTION:") {
            description = desc.trim().to_string();
            in_text_section = false;
        } else if let Some(title) = trimmed.strip_prefix("TITLE:") {
            suggested_title = title.trim().to_string();
            in_text_section = false;
        } else if let Some(tags) = trimmed.strip_prefix("TAGS:") {
            suggested_tags = tags
                .split(',')
                .map(|t| t.trim().to_lowercase().replace(' ', "-"))
                .filter(|t| !t.is_empty())
                .collect();
            in_text_section = false;
        } else if in_text_section && !trimmed.is_empty() {
            // Continuation of TEXT block
            if !extracted_text.is_empty() {
                extracted_text.push('\n');
            }
            extracted_text.push_str(trimmed);
        }
    }

    // Fallback: if parsing failed, use the whole output as description
    if description.is_empty() && !output.is_empty() {
        description = output.lines().take(3).collect::<Vec<_>>().join(" ");
    }

    VisionResult {
        description,
        suggested_title,
        suggested_tags,
        extracted_text,
    }
}

/// Determine MIME type from a filename extension.
pub fn mime_from_extension(filename: &str) -> String {
    let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tiff" | "tif" => "image/tiff",
        _ => "image/jpeg", // default assumption
    }
    .to_string()
}

#[cfg(test)]
mod tests;
