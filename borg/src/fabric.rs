use eyre::{Result, bail};
use std::process::Command;

use crate::config::FabricConfig;

#[derive(Debug)]
pub struct YouTubeContent {
    pub title: String,
    pub channel: String,
    pub duration_secs: f64,
    pub published_at: String,
    pub transcript: String,
    pub video_id: String,
    pub description: String,
    pub tags: Vec<String>,
}

pub async fn run_pattern(pattern: &str, input: &str, config: &FabricConfig) -> Result<String> {
    vault::fabric::run_pattern(pattern, input, &config.binary, &config.model, config.max_content_chars)
}

pub async fn fetch_youtube(url: &str, config: &FabricConfig) -> Result<YouTubeContent> {
    // Get metadata via fabric -y <url> --metadata
    let binary = vault::fabric::resolve_binary(&config.binary);
    log::debug!("fabric: fetching YouTube metadata for {url}");
    let mut cmd = Command::new(&binary);
    cmd.args(["-y", url, "--metadata"]);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let output = cmd.spawn()?.wait_with_output()?;
    let metadata_json = if output.status.success() {
        String::from_utf8_lossy(&output.stdout).to_string()
    } else {
        String::new()
    };

    // Parse metadata
    let (title, channel, duration_secs, published_at, video_id, description, tags) =
        parse_youtube_metadata(&metadata_json, url);

    // Get transcript via fabric -y <url> --transcript
    log::debug!("fabric: fetching YouTube transcript for {url}");
    let mut cmd = Command::new(&binary);
    cmd.args(["-y", url, "--transcript"]);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let output = cmd.spawn()?.wait_with_output()?;
    let transcript = if output.status.success() {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        log::warn!("fabric -y --transcript failed: {stderr}");
        String::new()
    };

    Ok(YouTubeContent {
        title,
        channel,
        duration_secs,
        published_at,
        transcript,
        video_id,
        description,
        tags,
    })
}

pub async fn fetch_article(url: &str, config: &FabricConfig) -> Result<String> {
    // Primary: fabric -u <url>
    let binary = vault::fabric::resolve_binary(&config.binary);
    log::debug!("fabric: fetching article for {url}");
    let mut cmd = Command::new(&binary);
    cmd.args(["-u", url]);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let output = cmd.spawn()?.wait_with_output()?;
    if output.status.success() {
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !text.is_empty() {
            return Ok(text);
        }
    }

    // Fallback: markitdown
    log::debug!("fabric -u failed, trying markitdown-cli for {url}");
    let output = Command::new("markitdown-cli")
        .arg(url)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|c| c.wait_with_output());

    if let Ok(output) = output
        && output.status.success()
    {
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !text.is_empty() {
            return Ok(text);
        }
    }

    // Last resort: jina.rs (caller handles this)
    bail!("Both fabric -u and markitdown-cli failed for {url}")
}

pub async fn summarize(content: &str, is_youtube: bool, config: &FabricConfig) -> Result<String> {
    let pattern = if is_youtube {
        &config.summarize_pattern_youtube
    } else {
        &config.summarize_pattern_article
    };
    run_pattern(pattern, content, config).await
}

pub async fn generate_tags(content: &str, config: &FabricConfig) -> Result<Vec<String>> {
    let output = run_pattern(&config.tag_pattern, content, config).await?;
    let tags: Vec<String> = output
        .split_whitespace()
        .map(|t| t.trim_matches('#').to_string())
        .filter(|t| !t.is_empty())
        .collect();
    Ok(tags)
}

fn parse_youtube_metadata(json_str: &str, url: &str) -> (String, String, f64, String, String, String, Vec<String>) {
    let video_id = crate::youtube::extract_video_id(url).unwrap_or_default();

    if let Ok(json) = serde_json::from_str::<serde_json::Value>(json_str) {
        let title = json["title"].as_str().unwrap_or("Unknown").to_string();
        let channel = json["channel"]
            .as_str()
            .or_else(|| json["uploader"].as_str())
            .unwrap_or("Unknown")
            .to_string();
        let duration = json["duration"].as_f64().unwrap_or(0.0);
        let published = json["upload_date"]
            .as_str()
            .or_else(|| json["published_at"].as_str())
            .unwrap_or("")
            .to_string();
        let description = json["description"].as_str().unwrap_or("").to_string();
        let tags = json["tags"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).map(|s| s.to_string()).collect())
            .unwrap_or_default();
        (title, channel, duration, published, video_id, description, tags)
    } else {
        (
            "Unknown".to_string(),
            "Unknown".to_string(),
            0.0,
            String::new(),
            video_id,
            String::new(),
            Vec::new(),
        )
    }
}

pub fn is_available(config: &FabricConfig) -> bool {
    vault::fabric::is_available(&config.binary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_youtube_metadata_valid() {
        let json = r#"{"title": "Test Video", "channel": "TestChan", "duration": 120.0, "upload_date": "2026-01-01", "description": "A test video", "tags": ["rust", "coding"]}"#;
        let (title, channel, dur, published, _vid, description, tags) =
            parse_youtube_metadata(json, "https://youtube.com/watch?v=abc123");
        assert_eq!(title, "Test Video");
        assert_eq!(channel, "TestChan");
        assert!((dur - 120.0).abs() < f64::EPSILON);
        assert_eq!(published, "2026-01-01");
        assert_eq!(description, "A test video");
        assert_eq!(tags, vec!["rust", "coding"]);
    }

    #[test]
    fn test_parse_youtube_metadata_invalid() {
        let (title, channel, dur, _, _, description, tags) =
            parse_youtube_metadata("not json", "https://youtube.com/watch?v=abc");
        assert_eq!(title, "Unknown");
        assert_eq!(channel, "Unknown");
        assert!((dur - 0.0).abs() < f64::EPSILON);
        assert!(description.is_empty());
        assert!(tags.is_empty());
    }
}
