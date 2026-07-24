use eyre::{Context, Result, bail};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use crate::config::{FabricConfig, PipelineConfig};

/// Wait for a child process with a per-call timeout. Kills the child on
/// elapsed and returns an error. Caller still owns the `Child`; on success
/// they call `wait_with_output()` to collect output.
///
/// This is the URL-fetch variant (own-the-`Child`, drain via
/// `wait_with_output`); the deadlock-safe stdin/stdout-draining primitive for
/// pattern invocations is `vault::fabric::wait_with_timeout`.
fn wait_with_timeout(child: &mut Child, timeout_secs: u64, label: &str) -> Result<()> {
    let timeout = Duration::from_secs(timeout_secs);
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    bail!("{label} timed out after {timeout_secs}s");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => bail!("Failed to wait for {label}: {e}"),
        }
    }
}

/// Resolve a pattern name to a file path.
///
/// If the pattern is already a path (starts with `~`, `/`, or `.`), return it as-is.
/// Otherwise, treat it as a filename and resolve to `~/.config/sb/patterns/<name>`.
/// If that file exists, return the resolved path. Otherwise, return the original name
/// so fabric can try its own pattern resolution as a fallback.
fn resolve_pattern(name: &str) -> String {
    if name.starts_with('~') || name.starts_with('/') || name.starts_with('.') {
        return name.to_string();
    }
    let path: PathBuf = vault::paths::patterns_dir().join(name);
    if path.exists() {
        return path.to_string_lossy().to_string();
    }
    name.to_string()
}

pub async fn run_pattern(pattern: &str, input: &str, config: &FabricConfig) -> Result<String> {
    let resolved = resolve_pattern(pattern);
    vault::fabric::run_pattern(
        &resolved,
        input,
        &config.binary,
        &config.api_key,
        &config.model,
        config.max_content_chars,
        config.timeout_secs,
    )
}

/// Fetch a YouTube transcript via fabric's captions API.
/// Returns the transcript text, or an empty string if unavailable.
/// Metadata is NOT fetched here - yt-dlp is the authoritative source for all metadata.
/// See docs/design/2026-03-22-youtube-metadata-pipeline-redesign.md.
///
/// Timeout is `pipeline.fabric_transcript_timeout_secs` - decoupled from
/// `config.fabric.timeout_secs` (which governs LLM pattern completions)
/// so a stuck transcript fetch can't burn the LLM budget.
pub fn fetch_transcript(url: &str, fabric: &FabricConfig, pipeline: &PipelineConfig) -> Result<String> {
    let binary = vault::fabric::resolve_binary(&fabric.binary);
    let timeout_secs = pipeline.fabric_transcript_timeout_secs;
    log::debug!("fabric: fetching YouTube transcript for {url} (timeout={timeout_secs}s)");
    let mut child = Command::new(&binary)
        .args(["-y", url, "--transcript"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("Failed to spawn fabric")?;

    if wait_with_timeout(&mut child, timeout_secs, "fabric -y --transcript").is_err() {
        return Ok(String::new());
    }
    let output = child.wait_with_output().context("Failed to collect fabric output")?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        log::warn!("fabric -y --transcript failed: {stderr}");
        Ok(String::new())
    }
}

/// Fetch article markdown by trying `fabric -u`, falling back to `markitdown`,
/// each bounded by its own pipeline-level timeout. Returns the first non-empty
/// extraction or bails so the caller can fall back further (Jina, etc.).
///
/// The two subprocess timeouts (`pipeline.fabric_url_timeout_secs` and
/// `pipeline.markitdown_timeout_secs`) are deliberately distinct from
/// `config.fabric.timeout_secs` (LLM completion). URL scrapes should
/// complete in under a minute; an LLM pattern call genuinely can need
/// several. Conflating them lets a hung scrape burn the LLM budget.
pub async fn fetch_article(url: &str, fabric: &FabricConfig, pipeline: &PipelineConfig) -> Result<String> {
    // The body is blocking (spawn + sync `wait_with_timeout` poll loop).
    // Run it on a blocking thread so it never stalls a tokio worker - the
    // previous direct call ran the 100ms-sleep poll loop on the async
    // runtime. `fetch_transcript` is already wrapped at its call site; this
    // brings `fetch_article` in line.
    let binary = vault::fabric::resolve_binary(&fabric.binary);
    let fabric_timeout = pipeline.fabric_url_timeout_secs;
    let markitdown_timeout = pipeline.markitdown_timeout_secs;
    let url = url.to_string();
    tokio::task::spawn_blocking(move || fetch_article_blocking(&url, &binary, fabric_timeout, markitdown_timeout))
        .await
        .context("fetch_article blocking task panicked")?
}

fn fetch_article_blocking(url: &str, binary: &str, fabric_timeout: u64, markitdown_timeout: u64) -> Result<String> {
    log::debug!("fabric: fetching article for {url} (timeout={fabric_timeout}s)");
    let mut child = Command::new(binary)
        .args(["-u", url])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("Failed to spawn fabric")?;

    if wait_with_timeout(&mut child, fabric_timeout, "fabric -u").is_ok() {
        match child.wait_with_output() {
            Ok(output) if output.status.success() => {
                let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if text.is_empty() {
                    log::warn!("fabric -u produced empty output for {url}");
                } else {
                    return Ok(text);
                }
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                log::warn!(
                    "fabric -u exited {} for {url}; stderr: {}",
                    output.status,
                    stderr.trim().chars().take(500).collect::<String>()
                );
            }
            Err(e) => log::warn!("fabric -u wait_with_output failed for {url}: {e}"),
        }
    }

    log::debug!("fabric -u failed, trying markitdown for {url} (timeout={markitdown_timeout}s)");
    match Command::new("markitdown")
        .arg(url)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(mut markitdown) => {
            if wait_with_timeout(&mut markitdown, markitdown_timeout, "markitdown").is_ok() {
                match markitdown.wait_with_output() {
                    Ok(output) if output.status.success() => {
                        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
                        if text.is_empty() {
                            log::warn!("markitdown produced empty output for {url}");
                        } else {
                            return Ok(text);
                        }
                    }
                    Ok(output) => {
                        let stderr = String::from_utf8_lossy(&output.stderr);
                        log::warn!(
                            "markitdown exited {} for {url}; stderr: {}",
                            output.status,
                            stderr.trim().chars().take(500).collect::<String>()
                        );
                    }
                    Err(e) => log::warn!("markitdown wait_with_output failed for {url}: {e}"),
                }
            }
        }
        Err(e) => log::warn!("failed to spawn markitdown for {url}: {e}"),
    }

    // Last resort: jina.rs (caller handles this)
    bail!("Both fabric -u and markitdown failed for {url}")
}

pub async fn summarize(content: &str, is_youtube: bool, config: &FabricConfig) -> Result<String> {
    let pattern = if is_youtube {
        &config.summarize_pattern_youtube
    } else {
        &config.summarize_pattern_article
    };

    if content.len() <= config.max_content_chars {
        return run_pattern(pattern, content, config).await;
    }

    log::info!(
        "Content exceeds max_content_chars ({} > {}), using multi-pass chunked summarization",
        content.len(),
        config.max_content_chars
    );
    summarize_chunked(content, pattern, config).await
}

/// Force chunked summarization regardless of content length.
/// Used when quality gate detects truncation artifacts in a single-pass summary.
pub async fn summarize_forced_chunked(content: &str, is_youtube: bool, config: &FabricConfig) -> Result<String> {
    let pattern = if is_youtube {
        &config.summarize_pattern_youtube
    } else {
        &config.summarize_pattern_article
    };
    log::info!("Forced chunked summarization for {} chars of content", content.len());
    summarize_chunked(content, pattern, config).await
}

/// Multi-pass summarization for content that exceeds max_content_chars.
/// 1. Split content into overlapping chunks that fit within the limit.
/// 2. Run the condense pattern on each chunk to extract key details.
/// 3. Concatenate condensed chunks and run the final summarize pattern.
fn summarize_chunked<'a>(
    content: &'a str,
    pattern: &'a str,
    config: &'a FabricConfig,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>> {
    Box::pin(async move {
        let chunk_size = config.max_content_chars;
        // 10% overlap to avoid losing context at chunk boundaries
        let overlap = chunk_size / 10;
        let chunks = split_with_overlap(content, chunk_size, overlap);

        log::info!(
            "Split {} chars into {} chunks (chunk_size={}, overlap={})",
            content.len(),
            chunks.len(),
            chunk_size,
            overlap
        );

        // Condense each chunk in sequence (parallel would hammer the LLM)
        let mut condensed_parts = Vec::with_capacity(chunks.len());
        for (i, chunk) in chunks.iter().enumerate() {
            log::info!("Condensing chunk {}/{} ({} chars)", i + 1, chunks.len(), chunk.len());
            let condensed = run_pattern(&config.condense_pattern, chunk, config).await?;
            condensed_parts.push(condensed);
        }

        let merged = condensed_parts.join("\n\n---\n\n");
        log::info!(
            "Condensed {} chars down to {} chars, running final summarization",
            content.len(),
            merged.len()
        );

        // If the condensed result still exceeds the limit, recurse
        if merged.len() > config.max_content_chars {
            log::warn!(
                "Condensed output still exceeds limit ({} > {}), recursing",
                merged.len(),
                config.max_content_chars
            );
            return summarize_chunked(&merged, pattern, config).await;
        }

        run_pattern(pattern, &merged, config).await
    })
}

/// Split text into chunks of approximately `chunk_size` chars with `overlap` char overlap.
/// Tries to split at paragraph boundaries (\n\n) or sentence boundaries (. ) for cleaner chunks.
fn split_with_overlap(text: &str, chunk_size: usize, overlap: usize) -> Vec<String> {
    // chunk_size == 0 would never advance; treat as "no chunking".
    if chunk_size == 0 || text.len() <= chunk_size {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut start = 0;

    while start < text.len() {
        // Snap the cut to a char boundary; a raw byte cut panicked when it
        // landed inside a multi-byte codepoint.
        let end = text.floor_char_boundary((start + chunk_size).min(text.len()));

        // Try to find a clean break point near the end
        let mut actual_end = if end < text.len() {
            find_break_point(text, end.saturating_sub(200), end)
        } else {
            end
        };

        // find_break_point's 200-byte lookback can precede `start` for small
        // chunk sizes and return an offset <= start; never slice backwards or
        // stall - advance at least one char.
        if actual_end <= start {
            actual_end = text.ceil_char_boundary((start + 1).min(text.len()));
        }

        chunks.push(text[start..actual_end].to_string());

        if actual_end >= text.len() {
            break;
        }

        // Next chunk starts `overlap` chars before the end of this one, but
        // must always move forward.
        let next = text.floor_char_boundary(actual_end.saturating_sub(overlap));
        start = if next > start { next } else { actual_end };
    }

    chunks
}

/// Find a clean break point (paragraph or sentence boundary) in the range [search_start, end].
/// Falls back to `end` if no good break point is found.
fn find_break_point(text: &str, search_start: usize, end: usize) -> usize {
    // Both bounds must sit on char boundaries before slicing; callers pass
    // byte-arithmetic offsets that may land mid-codepoint.
    let search_start = text.floor_char_boundary(search_start);
    let end = text.floor_char_boundary(end);
    let region = &text[search_start..end];

    // Prefer paragraph breaks
    if let Some(pos) = region.rfind("\n\n") {
        return search_start + pos + 2;
    }
    // Fall back to sentence breaks
    if let Some(pos) = region.rfind(". ") {
        return search_start + pos + 2;
    }
    // Fall back to line breaks
    if let Some(pos) = region.rfind('\n') {
        return search_start + pos + 1;
    }
    end
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

#[cfg(test)]
mod tests;
