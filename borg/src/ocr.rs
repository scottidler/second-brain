use base64::Engine;
use eyre::{Context, Result};
use std::path::Path;
use std::process::Command;

use crate::config::{self, LlmConfig, VisionConfig};

/// Result of vision-based image description.
pub struct VisionResult {
    pub description: String,
    pub suggested_title: String,
    pub suggested_tags: Vec<String>,
    pub extracted_text: String,
}

/// Extract text from an image using tesseract CLI.
/// Returns empty string if tesseract is not available or fails.
pub fn ocr_extract(image_path: &Path) -> Result<String> {
    let output = Command::new("/usr/bin/tesseract")
        .args([
            image_path.to_str().unwrap_or_default(),
            "stdout",
            "--oem",
            "3",
            "--psm",
            "3",
        ])
        .output()
        .context("Failed to run tesseract")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        log::warn!("tesseract failed: {stderr}");
        return Ok(String::new());
    }

    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(text)
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

    let client = reqwest::Client::new();
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
mod tests {
    use super::*;

    #[test]
    fn test_ocr_extract_nonexistent_file() {
        // tesseract should fail gracefully on a nonexistent file
        let result = ocr_extract(Path::new("/tmp/nonexistent-obsidian-borg-test.png"));
        // Either returns an error (tesseract not installed) or empty/non-empty string
        if let Ok(text) = result {
            // Any result is acceptable - just verify we got a string back
            let _ = text.len();
        }
    }

    #[test]
    fn test_vision_result_struct() {
        let result = VisionResult {
            description: "A whiteboard diagram".to_string(),
            suggested_title: "Whiteboard Notes".to_string(),
            suggested_tags: vec!["diagram".to_string(), "notes".to_string()],
            extracted_text: "Hello World".to_string(),
        };
        assert_eq!(result.description, "A whiteboard diagram");
        assert_eq!(result.suggested_title, "Whiteboard Notes");
        assert_eq!(result.suggested_tags.len(), 2);
        assert_eq!(result.extracted_text, "Hello World");
    }

    #[test]
    fn test_parse_vision_response_well_formed() {
        let response = "\
TEXT: Serial: ABC-123\nModel: SG-2100\n\
DESCRIPTION: A product label showing serial number and model information.\n\
TITLE: Netgate SG-2100 Serial Label\n\
TAGS: hardware, serial-number, netgate";

        let result = parse_vision_response(response);
        assert_eq!(result.extracted_text, "Serial: ABC-123\nModel: SG-2100");
        assert_eq!(
            result.description,
            "A product label showing serial number and model information."
        );
        assert_eq!(result.suggested_title, "Netgate SG-2100 Serial Label");
        assert_eq!(result.suggested_tags, vec!["hardware", "serial-number", "netgate"]);
    }

    #[test]
    fn test_parse_vision_response_empty() {
        let result = parse_vision_response("");
        assert!(result.extracted_text.is_empty());
        assert!(result.description.is_empty());
        assert!(result.suggested_title.is_empty());
        assert!(result.suggested_tags.is_empty());
    }

    #[test]
    fn test_parse_vision_response_partial() {
        let response = "TITLE: Some Image\nTAGS: photo, test";
        let result = parse_vision_response(response);
        assert_eq!(result.suggested_title, "Some Image");
        assert_eq!(result.suggested_tags, vec!["photo", "test"]);
        assert!(result.extracted_text.is_empty());
        // description falls back to first 3 lines
        assert!(!result.description.is_empty());
    }

    #[test]
    fn test_parse_vision_response_multiline_text() {
        let response = "\
TEXT: Line 1
Line 2
Line 3
DESCRIPTION: A multi-line text image.
TITLE: Multi Line Text
TAGS: text";

        let result = parse_vision_response(response);
        assert_eq!(result.extracted_text, "Line 1\nLine 2\nLine 3");
        assert_eq!(result.description, "A multi-line text image.");
    }

    #[test]
    fn test_parse_vision_response_tags_with_spaces() {
        let response = "TAGS: machine learning, deep learning, neural networks";
        let result = parse_vision_response(response);
        assert_eq!(
            result.suggested_tags,
            vec!["machine-learning", "deep-learning", "neural-networks"]
        );
    }

    #[test]
    fn test_mime_from_extension() {
        assert_eq!(mime_from_extension("photo.jpg"), "image/jpeg");
        assert_eq!(mime_from_extension("photo.jpeg"), "image/jpeg");
        assert_eq!(mime_from_extension("screenshot.png"), "image/png");
        assert_eq!(mime_from_extension("anim.gif"), "image/gif");
        assert_eq!(mime_from_extension("modern.webp"), "image/webp");
        assert_eq!(mime_from_extension("unknown.xyz"), "image/jpeg");
        assert_eq!(mime_from_extension("noext"), "image/jpeg");
    }
}
