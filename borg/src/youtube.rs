use eyre::{Context, Result, bail};
use std::process::Command;
use std::sync::LazyLock;

static VIDEO_ID_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?:youtube\.com/watch\?v=|youtu\.be/|youtube\.com/shorts/|youtube\.com/embed/)([a-zA-Z0-9_-]{11})",
    )
    .expect("valid regex")
});

pub fn extract_video_id(url: &str) -> Option<String> {
    VIDEO_ID_REGEX
        .captures(url)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

pub fn generate_embed_code(video_id: &str, width: usize, height: usize) -> String {
    format!(
        r#"<iframe width="{width}" height="{height}" src="https://www.youtube.com/embed/{video_id}" frameborder="0" allowfullscreen></iframe>"#
    )
}

#[derive(Debug)]
pub struct VideoMetadata {
    pub title: String,
    pub uploader: String,
    pub duration_secs: f64,
}

pub fn fetch_metadata(url: &str) -> Result<VideoMetadata> {
    log::debug!("yt-dlp: fetching metadata for {url}");
    let output = Command::new("yt-dlp")
        .args(["--dump-json", "--no-download", "--no-warnings", url])
        .output()
        .context("Failed to run yt-dlp - is it installed?")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        log::error!("yt-dlp metadata failed (exit {}): {stderr}", output.status);
        bail!("yt-dlp failed: {stderr}");
    }
    log::debug!(
        "yt-dlp: metadata fetch succeeded ({} bytes stdout)",
        output.stdout.len()
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).context("Failed to parse yt-dlp JSON")?;

    Ok(VideoMetadata {
        title: json["title"].as_str().unwrap_or("Unknown").to_string(),
        uploader: json["uploader"].as_str().unwrap_or("Unknown").to_string(),
        duration_secs: json["duration"].as_f64().unwrap_or(0.0),
    })
}

pub async fn fetch_subtitles(url: &str) -> Result<Option<String>> {
    log::debug!("yt-dlp: fetching subtitles for {url}");
    let output = Command::new("yt-dlp")
        .args([
            "--write-auto-sub",
            "--sub-lang",
            "en",
            "--sub-format",
            "vtt",
            "--skip-download",
            "--print",
            "%(requested_subtitles)j",
            url,
        ])
        .output()
        .context("Failed to run yt-dlp for subtitles")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        log::debug!("yt-dlp subtitles failed (exit {}): {stderr}", output.status);
        return Ok(None);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let trimmed = stdout.trim();
    log::debug!("yt-dlp subtitles output: {trimmed}");

    if trimmed == "NA" || trimmed == "null" || trimmed.is_empty() {
        log::debug!("No subtitles available (output was: {trimmed})");
        return Ok(None);
    }

    // Try to get the subtitle content from the JSON
    let subs: serde_json::Value = serde_json::from_str(trimmed).unwrap_or_default();
    log::debug!(
        "Parsed subtitles JSON keys: {:?}",
        subs.as_object().map(|o| o.keys().collect::<Vec<_>>())
    );
    if let Some(en_sub) = subs.get("en") {
        // Prefer local filepath if yt-dlp wrote the file
        if let Some(filepath) = en_sub.get("filepath").and_then(|f| f.as_str()) {
            log::debug!("Reading subtitle file: {filepath}");
            let content = std::fs::read_to_string(filepath).context("Failed to read subtitle file")?;
            let cleaned = clean_vtt(&content);
            log::debug!("Subtitle file read and cleaned: {} chars", cleaned.len());
            let _ = std::fs::remove_file(filepath);
            return Ok(Some(cleaned));
        }
        // Fall back to downloading from the URL
        if let Some(sub_url) = en_sub.get("url").and_then(|u| u.as_str()) {
            log::debug!("Downloading subtitles from URL: {sub_url}");
            let response = reqwest::get(sub_url).await.context("Failed to download subtitle VTT")?;
            if response.status().is_success() {
                let content = response.text().await.context("Failed to read subtitle response")?;
                let cleaned = clean_vtt(&content);
                log::debug!("Downloaded and cleaned subtitles: {} chars", cleaned.len());
                return Ok(Some(cleaned));
            }
            log::warn!("Subtitle download returned status {}", response.status());
        }
    }

    log::debug!("No usable 'en' subtitle entry found in JSON");
    Ok(None)
}

pub fn extract_audio(url: &str, output_dir: &str) -> Result<String> {
    log::debug!("yt-dlp: extracting audio for {url} to {output_dir}");
    let output_template = format!("{output_dir}/%(id)s.%(ext)s");

    let output = Command::new("yt-dlp")
        .args([
            "-x",
            "--audio-format",
            "mp3",
            "--audio-quality",
            "5",
            "-o",
            &output_template,
            url,
        ])
        .output()
        .context("Failed to run yt-dlp for audio extraction")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        log::error!("yt-dlp audio extraction failed (exit {}): {stderr}", output.status);
        bail!("yt-dlp audio extraction failed: {stderr}");
    }

    // Find the output file
    let stdout = String::from_utf8_lossy(&output.stdout);
    log::debug!("yt-dlp audio extraction stdout:\n{stdout}");
    for line in stdout.lines() {
        if line.contains("[ExtractAudio] Destination:")
            && let Some(path) = line.split("Destination:").nth(1)
        {
            return Ok(path.trim().to_string());
        }
    }

    bail!("Could not determine audio output path from yt-dlp")
}

fn clean_vtt(vtt: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut last_line = String::new();

    for line in vtt.lines() {
        let line = line.trim();

        // Skip VTT headers and timestamps
        if line.starts_with("WEBVTT")
            || line.starts_with("Kind:")
            || line.starts_with("Language:")
            || line.contains("-->")
            || line.is_empty()
        {
            continue;
        }

        // Skip numeric cue identifiers
        if line.parse::<u32>().is_ok() {
            continue;
        }

        // Remove HTML tags
        let cleaned = line
            .replace("<c>", "")
            .replace("</c>", "")
            .replace("<i>", "")
            .replace("</i>", "");

        // Deduplicate consecutive identical lines
        if cleaned != last_line {
            lines.push(cleaned.clone());
            last_line = cleaned;
        }
    }

    lines.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_vtt_removes_headers() {
        let vtt = "WEBVTT\nKind: captions\nLanguage: en\n\n00:00:00.000 --> 00:00:05.000\nHello world\n\n00:00:05.000 --> 00:00:10.000\nThis is a test";
        let result = clean_vtt(vtt);
        assert_eq!(result, "Hello world This is a test");
    }

    #[test]
    fn test_clean_vtt_removes_html_tags() {
        let vtt = "00:00:00.000 --> 00:00:05.000\n<c>Hello</c> <i>world</i>";
        let result = clean_vtt(vtt);
        assert_eq!(result, "Hello world");
    }

    #[test]
    fn test_clean_vtt_deduplicates() {
        let vtt = "00:00:00.000 --> 00:00:05.000\nHello\n\n00:00:05.000 --> 00:00:10.000\nHello\n\n00:00:10.000 --> 00:00:15.000\nWorld";
        let result = clean_vtt(vtt);
        assert_eq!(result, "Hello World");
    }

    #[test]
    fn test_extract_video_id_watch() {
        let id = extract_video_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ");
        assert_eq!(id, Some("dQw4w9WgXcQ".to_string()));
    }

    #[test]
    fn test_extract_video_id_short() {
        let id = extract_video_id("https://youtu.be/dQw4w9WgXcQ");
        assert_eq!(id, Some("dQw4w9WgXcQ".to_string()));
    }

    #[test]
    fn test_extract_video_id_shorts() {
        let id = extract_video_id("https://youtube.com/shorts/dQw4w9WgXcQ");
        assert_eq!(id, Some("dQw4w9WgXcQ".to_string()));
    }

    #[test]
    fn test_extract_video_id_none() {
        let id = extract_video_id("https://example.com/page");
        assert_eq!(id, None);
    }

    #[test]
    fn test_generate_embed_code() {
        let code = generate_embed_code("abc123_-XYZ", 854, 480);
        assert!(code.contains("abc123_-XYZ"));
        assert!(code.contains("854"));
        assert!(code.contains("480"));
        assert!(code.contains("iframe"));
    }
}
