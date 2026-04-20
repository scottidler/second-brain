use eyre::{Context, Result};

use crate::stages::fetcher::{BrowserUaFetcher, Fetcher};

/// Fetch article markdown. Primary path is Jina Reader (r.jina.ai).
/// On HTTP 451 (Jina IP-block) or any other failure, fall back to a direct
/// reqwest with a realistic browser User-Agent piped through markitdown-cli.
/// This recovers URLs whose origin blocks Jina's IP range (e.g. XDA, HowToGeek
/// circa 2026-04-19) but happily serves requests that look like a browser.
pub async fn fetch_article_markdown(url: &str) -> Result<String> {
    match jina_fetch(url).await {
        Ok(text) => Ok(text),
        Err(e) => {
            log::warn!("jina: failed for {url} ({e:#}); falling back to browser-UA");
            let browser = BrowserUaFetcher::new();
            let result = browser
                .fetch(url)
                .await
                .with_context(|| format!("browser-UA fallback also failed for {url}"))?;
            let text = String::from_utf8_lossy(&result.bytes).to_string();
            Ok(text)
        }
    }
}

async fn jina_fetch(url: &str) -> Result<String> {
    let client = reqwest::Client::new();
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
mod tests {
    #[test]
    fn test_jina_url_format() {
        let url = "https://blog.example.com/post";
        let jina_url = format!("https://r.jina.ai/{url}");
        assert_eq!(jina_url, "https://r.jina.ai/https://blog.example.com/post");
    }
}
