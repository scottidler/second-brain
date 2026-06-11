use eyre::{Context, Result};
use std::time::Duration;

use crate::stages::fetcher::{BrowserUaFetcher, Fetcher};

/// Fetch article markdown plus an optional byline. Primary path is Jina Reader
/// (r.jina.ai) in markdown mode, which exposes no author and so yields `None`.
/// On HTTP 451 (Jina IP-block) or any other failure, fall back to a direct
/// reqwest with a realistic browser User-Agent piped through markitdown; that
/// `BrowserUaFetcher` runs `byline::extract` on the raw HTML and surfaces
/// `meta.author`. This recovers URLs whose origin blocks Jina's IP range (e.g.
/// XDA, HowToGeek circa 2026-04-19) but happily serves browser-looking
/// requests - and is the only live blog path that can currently carry a byline.
///
/// A Jina-JSON author source is a separate, in-progress workstream; when it
/// lands it composes here as `json_author.or(browser_byline)`.
pub async fn fetch_article_markdown(url: &str, timeout_secs: u64) -> Result<(String, Option<String>)> {
    match jina_fetch(url, timeout_secs).await {
        Ok(text) => Ok((text, None)),
        Err(e) => {
            log::warn!("jina: failed for {url} ({e:#}); falling back to browser-UA");
            let browser = BrowserUaFetcher::new();
            let result = browser
                .fetch(url)
                .await
                .with_context(|| format!("browser-UA fallback also failed for {url}"))?;
            let text = String::from_utf8_lossy(&result.bytes).to_string();
            Ok((text, result.meta.author))
        }
    }
}

async fn jina_fetch(url: &str, timeout_secs: u64) -> Result<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .context("Failed to build Jina reqwest client")?;
    let jina_url = format!("https://r.jina.ai/{url}");

    let response = client
        .get(&jina_url)
        .header("Accept", "text/markdown")
        .send()
        .await
        .context("Failed to reach Jina Reader")?;

    if !response.status().is_success() {
        eyre::bail!(
            "Jina Reader returned status {}: {}",
            response.status(),
            response.text().await.unwrap_or_default()
        );
    }

    response
        .text()
        .await
        .context("Failed to read Jina Reader response body")
}

#[cfg(test)]
mod tests;
