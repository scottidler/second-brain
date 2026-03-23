use eyre::{Result, bail};
use std::path::PathBuf;
use std::process::Command;

use crate::config::FabricConfig;

/// Resolve a pattern name to a file path.
///
/// If the pattern is already a path (starts with `~`, `/`, or `.`), return it as-is.
/// Otherwise, treat it as a filename and resolve to `~/.config/borg/patterns/<name>`.
/// If that file exists, return the resolved path. Otherwise, return the original name
/// so fabric can try its own pattern resolution as a fallback.
fn resolve_pattern(name: &str) -> String {
    if name.starts_with('~') || name.starts_with('/') || name.starts_with('.') {
        return name.to_string();
    }
    if let Some(home) = dirs::home_dir() {
        let path: PathBuf = home.join(".config/borg/patterns").join(name);
        if path.exists() {
            return path.to_string_lossy().to_string();
        }
    }
    name.to_string()
}

pub async fn run_pattern(pattern: &str, input: &str, config: &FabricConfig) -> Result<String> {
    let resolved = resolve_pattern(pattern);
    vault::fabric::run_pattern(
        &resolved,
        input,
        &config.binary,
        &config.model,
        config.max_content_chars,
    )
}

/// Fetch a YouTube transcript via fabric's captions API.
/// Returns the transcript text, or an empty string if unavailable.
/// Metadata is NOT fetched here - yt-dlp is the authoritative source for all metadata.
/// See docs/design/2026-03-22-youtube-metadata-pipeline-redesign.md.
pub fn fetch_transcript(url: &str, config: &FabricConfig) -> Result<String> {
    let binary = vault::fabric::resolve_binary(&config.binary);
    log::debug!("fabric: fetching YouTube transcript for {url}");
    let mut cmd = Command::new(&binary);
    cmd.args(["-y", url, "--transcript"]);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let output = cmd.spawn()?.wait_with_output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        log::warn!("fabric -y --transcript failed: {stderr}");
        Ok(String::new())
    }
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

pub fn is_available(config: &FabricConfig) -> bool {
    vault::fabric::is_available(&config.binary)
}
