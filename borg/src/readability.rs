//! In-process clean-article extraction: fetch HTML with a browser UA, extract
//! the main article with `dom_smoothie` (a pure-Rust Mozilla-Readability port),
//! and render it to markdown with `htmd`. No external binary or language
//! runtime - this replaces the earlier shell-out to the `defuddle` Node CLI, so
//! there is no PATH / daemon-reachability problem.

use std::time::Duration;

use thiserror::Error;

/// A realistic desktop Firefox UA. Some sites gate plain/library UAs but serve
/// browser-looking clients; mirrors `stages::fetcher::BROWSER_UA`.
const BROWSER_UA: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:120.0) Gecko/20100101 Firefox/120.0";

/// Typed failure modes for [`fetch_article_readable`]. The caller falls through
/// to the existing fabric-u/Jina/browser-UA chain on ANY variant, matching on
/// the enum rather than string-sniffing a message.
#[derive(Debug, Error)]
pub enum ReadableError {
    #[error("readable fetch failed for {url}: {source}")]
    Fetch {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("readable fetch for {url} returned HTTP {status}")]
    Status { url: String, status: u16 },
    #[error("readability extraction failed for {url}: {reason}")]
    Extract { url: String, reason: String },
    #[error("readability produced empty content for {url}")]
    Empty { url: String },
}

/// Fetch `url` with a browser UA, extract the main article (`dom_smoothie`), and
/// render it to markdown (`htmd`). `timeout_secs` bounds the HTTP fetch. DOM
/// parsing + markdown rendering are CPU-bound and synchronous, so they run on a
/// blocking thread (never on the async runtime, per subprocess/async hygiene).
/// Returns the article markdown, or a typed [`ReadableError`] the caller treats
/// as "fall through to the existing fetch chain".
pub async fn fetch_article_readable(url: &str, timeout_secs: u64) -> Result<String, ReadableError> {
    log::debug!("fetch_article_readable: url={url} timeout_secs={timeout_secs}");

    let client = reqwest::Client::builder()
        .user_agent(BROWSER_UA)
        .redirect(reqwest::redirect::Policy::limited(5))
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .map_err(|source| ReadableError::Fetch {
            url: url.to_string(),
            source,
        })?;

    let response = client.get(url).send().await.map_err(|source| ReadableError::Fetch {
        url: url.to_string(),
        source,
    })?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(ReadableError::Status {
            url: url.to_string(),
            status,
        });
    }
    let html = response.text().await.map_err(|source| ReadableError::Fetch {
        url: url.to_string(),
        source,
    })?;

    let url_owned = url.to_string();
    let markdown = match tokio::task::spawn_blocking(move || extract_markdown(&html, &url_owned)).await {
        Ok(result) => result?,
        Err(join) => {
            return Err(ReadableError::Extract {
                url: url.to_string(),
                reason: format!("extraction task failed: {join}"),
            });
        }
    };

    if markdown.trim().is_empty() {
        return Err(ReadableError::Empty { url: url.to_string() });
    }
    log::debug!(
        "fetch_article_readable: url={url} produced {} chars",
        markdown.chars().count()
    );
    Ok(markdown)
}

/// Sync extraction: `dom_smoothie` (HTML -> clean article HTML) -> `htmd`.
/// Separated from the async fetch so it is unit-testable over a static HTML
/// fixture without network.
fn extract_markdown(html: &str, url: &str) -> Result<String, ReadableError> {
    let mut readability =
        dom_smoothie::Readability::new(html, Some(url), None).map_err(|e| ReadableError::Extract {
            url: url.to_string(),
            reason: e.to_string(),
        })?;
    let article = readability.parse().map_err(|e| ReadableError::Extract {
        url: url.to_string(),
        reason: e.to_string(),
    })?;
    let content_html = article.content.to_string();
    htmd::convert(&content_html).map_err(|e| ReadableError::Extract {
        url: url.to_string(),
        reason: e.to_string(),
    })
}

#[cfg(test)]
mod tests;
